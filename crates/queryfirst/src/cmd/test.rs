//! `qf test [--db <conn>] [--manifest <file>] [--strict] [paths…]`: re-describe every committed
//! query against a target database (in a deployment: the one the live app uses) and report every
//! way it differs from the meta (spec §9, §9a).
//!
//! * Metas come from the tree (`*.qf.json`, never the query files) or from a manifest written
//!   by `qf bundle`. With `--manifest` neither a `queryfirst.json` nor any source is needed; one
//!   in the current directory (or `--project`) still supplies `test.connection` and `timestamp`.
//! * Each meta's `statement.db` is described and the analyzers run against the target catalog.
//!   The meta's own declarations (alias suffixes, `:p!` / `:p?`, pick keys' `!`, `@type`) are
//!   re-applied there, so they never show up as differences. Then [`qf_meta::compare`].
//! * `QF049`: the meta records a broken query, or cannot be read, or its statements disagree.
//!   `QF046`: the statement does not prepare on the target. `QF048`: `timestamp` is `"utc"` and
//!   the session time zone is not UTC (see [`utc_timezone`]).
//! * Exit 1 on any error-severity difference, or on any difference with `--strict`; exit 2 on a
//!   connection, configuration or usage problem. The first connection error aborts the run.
//! * Read-only: only describes (Parse/Describe, never Execute) and the catalog `SELECT`s.
//! * The connection string is never printed: diagnostics name only `host:port`.

use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use qf_meta::{QueryMeta, Status, compare, manifest_from_json};
use qf_pg::{
    Catalog, ConnError, ConnSource, connect_pool, is_connection_error, resolve_connection,
    to_diagnostic,
};
use qf_sqlfile::{Severity, lex_params};
use serde_json::{Value, json};

use crate::cmd::bundle::read_metas;
use crate::cmd::generate::{CONNECT_TIMEOUT, connection_diagnostic};
use crate::config::{CONFIG_FILE, TestConfig, load_config};
use crate::discover::discover_metas;
use crate::engine::{Describe, assemble, connection_lost, query_file_from_meta};
use crate::posix;
use crate::report::{Report, ReportedDiagnostic, plural, render_diagnostics, to_json};

const REGENERATE: &str = "run `qf generate` and commit the result";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestOptions {
    /// Fail on any difference, notes and warnings included.
    pub strict: bool,
    /// The `timestamp` setting; `"utc"` enables the time zone check.
    pub timestamp: Option<String>,
    /// How many describes run at once.
    pub pool: usize,
}

/// One committed query to test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestEntry {
    /// Where it came from, for reports when the meta cannot be read: the meta file's
    /// project-relative path, or the query's source in a manifest.
    pub file: String,
    /// The meta, or why it cannot be read.
    pub meta: Result<QueryMeta, String>,
}

/// The outcome of `qf test`: its report, and whether `--strict` was given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestReport {
    /// Diagnostics grouped by query file (project-wide ones first), each query's differences in
    /// the order [`qf_meta::compare`] gives them.
    pub report: Report,
    pub strict: bool,
}

fn count(r: &Report, s: Severity) -> usize {
    r.diagnostics.iter().filter(|d| d.severity == s).count()
}

impl TestReport {
    /// 2 on connection, configuration or usage errors; 1 on any error, or on any difference with
    /// `--strict`; else 0.
    pub fn exit_code(&self) -> u8 {
        match self.report.exit_code() {
            0 if self.strict && !self.report.diagnostics.is_empty() => 1,
            code => code,
        }
    }

    /// Queries with at least one difference.
    pub fn differing(&self) -> usize {
        let mut files: Vec<&str> = self
            .report
            .diagnostics
            .iter()
            .filter_map(|d| d.file.as_deref())
            .collect();
        files.dedup();
        files.len()
    }

    /// The diagnostics, then `Queryfirst test: …` (for stderr).
    pub fn render_human(&self) -> String {
        let r = &self.report;
        let mut out = render_diagnostics(r);
        out.push_str(&format!(
            "Queryfirst test: {}, {} with differences; {}, {}, {}{}\n",
            plural(r.summary.queries, "query", "queries"),
            self.differing(),
            plural(r.summary.errors, "error", "errors"),
            plural(r.summary.warnings, "warning", "warnings"),
            plural(count(r, Severity::Note), "note", "notes"),
            if self.strict { " (strict)" } else { "" },
        ));
        out
    }

    /// The common report shape, with `ok` following the exit code, `strict`, and the summary's
    /// `notes` and `differing` counts.
    pub fn to_json(&self) -> Value {
        let mut v = to_json(&self.report);
        v["ok"] = json!(self.exit_code() == 0);
        v["strict"] = json!(self.strict);
        v["summary"]["notes"] = json!(count(&self.report, Severity::Note));
        v["summary"]["differing"] = json!(self.differing());
        v
    }

    fn print(&self, json: bool) {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&self.to_json()).expect("report serialises")
            );
        } else {
            eprint!("{}", self.render_human());
        }
    }
}

/// Whether a `TimeZone` setting is UTC. Deliberately narrow: only UTC itself and the tz
/// database names that are links to it (`Etc/UTC`, `UCT`, `Universal`, `Zulu` and their `Etc/`
/// forms), in any case. `GMT`, `Etc/GMT0`, `+00` and POSIX specs such as `UTC0` are not
/// accepted, though they have a zero offset: configure the database with `UTC`.
pub fn utc_timezone(tz: &str) -> bool {
    const UTC: [&str; 8] = [
        "UTC",
        "Etc/UTC",
        "UCT",
        "Etc/UCT",
        "Universal",
        "Etc/Universal",
        "Zulu",
        "Etc/Zulu",
    ];
    UTC.iter().any(|n| n.eq_ignore_ascii_case(tz.trim()))
}

/// The connection sources for `qf test`, first match wins: `--db`, the `test.connection.env`
/// variable, `DATABASE_URL` / the `PG*` variables, then the `test.connection.command` helper.
pub fn test_connection_sources(test: Option<&TestConfig>, db: Option<&str>) -> Vec<ConnSource> {
    let conn = test.and_then(|t| t.connection.as_ref());
    let mut sources = Vec::new();
    if let Some(db) = db {
        sources.push(ConnSource::Explicit(db.to_string()));
    }
    if let Some(env) = conn.and_then(|c| c.env.as_ref()) {
        sources.push(ConnSource::Env(env.clone()));
    }
    sources.push(ConnSource::StandardEnv);
    if let Some(cmd) = conn.and_then(|c| c.command.as_ref()) {
        sources.push(ConnSource::Command(cmd.clone()));
    }
    sources
}

fn diag(file: &str, code: &str, severity: Severity, message: String) -> ReportedDiagnostic {
    ReportedDiagnostic {
        file: Some(file.to_string()),
        line: 0,
        col: 0,
        code: code.to_string(),
        severity,
        message,
        hint: None,
    }
}

fn with_hint(mut d: ReportedDiagnostic, hint: Option<String>) -> ReportedDiagnostic {
    d.hint = hint;
    d
}

fn meta_severity(s: qf_meta::Severity) -> Severity {
    match s {
        qf_meta::Severity::Error => Severity::Error,
        qf_meta::Severity::Warning => Severity::Warning,
        qf_meta::Severity::Note => Severity::Note,
    }
}

/// Every difference between `meta` and the target. Only a connection error is an `Err`.
async fn test_meta<D: Describe>(
    meta: &QueryMeta,
    db: &D,
    cat: &Catalog,
) -> Result<Vec<ReportedDiagnostic>, ConnError> {
    let file = meta.source.as_str();
    let name = meta.name.as_str();
    if let Status::Broken { diagnostics } = &meta.status {
        let detail = diagnostics
            .first()
            .map_or(String::new(), |d| format!(": {} {}", d.code, d.message));
        let d = diag(
            file,
            "QF049",
            Severity::Error,
            format!("{name}: recorded broken by `qf generate`{detail}"),
        );
        let hint = "fix the query, run `qf generate` and deploy again".to_string();
        return Ok(vec![with_hint(d, Some(hint))]);
    }

    // The statement as the meta records it; `statement.db` is what gets described.
    let q = query_file_from_meta(meta);
    let lexed = match lex_params(&q) {
        Ok(l) if l.db_statement == meta.statement.db => l,
        _ => {
            let d = diag(
                file,
                "QF049",
                Severity::Error,
                format!(
                    "{name}: the meta's statement.source and statement.db disagree (edited by hand?)"
                ),
            );
            return Ok(vec![with_hint(d, Some(REGENERATE.into()))]);
        }
    };
    let described = match db.describe(&meta.statement.db).await {
        Ok(d) => d,
        Err(e) if is_connection_error(&e) => return Err(connection_lost(file, &e)),
        Err(e) => {
            let pg = to_diagnostic(&e, &lexed, cat);
            let d = diag(
                file,
                "QF046",
                Severity::Error,
                format!(
                    "{name}: does not prepare on the target database: {}",
                    pg.message
                ),
            );
            return Ok(vec![with_hint(d, pg.hint)]);
        }
    };

    let mut actual = meta.clone();
    actual.params.clear();
    actual.columns.clear();
    let mut diags = Vec::new();
    let assembled = assemble(&q, "", &lexed, &described, cat, &mut actual, &mut diags);
    let mut out: Vec<ReportedDiagnostic> = diags
        .iter()
        // QF004: an `@type` target the target database no longer has; compare reports it.
        .filter(|d| d.severity == Severity::Error && d.code != "QF004")
        .map(|d| {
            let code = if d.code == "QF010" { "QF046" } else { d.code };
            let msg = format!("{name}: on the target database: {}", d.message);
            with_hint(diag(file, code, Severity::Error, msg), d.hint.clone())
        })
        .collect();
    if assembled {
        out.extend(
            compare(meta, &actual.params, &actual.columns)
                .into_iter()
                .map(|d| diag(file, d.code, meta_severity(d.severity), d.message)),
        );
    }
    Ok(out)
}

async fn test_entry<D: Describe>(
    e: &TestEntry,
    db: &D,
    cat: &Catalog,
) -> Result<Vec<ReportedDiagnostic>, ConnError> {
    match &e.meta {
        Ok(meta) => test_meta(meta, db, cat).await,
        Err(why) => {
            let d = diag(
                &e.file,
                "QF049",
                Severity::Error,
                format!("{} cannot be read: {why}", e.file),
            );
            Ok(vec![with_hint(d, Some(REGENERATE.into()))])
        }
    }
}

/// Stable-sort by file (project-wide diagnostics first) and count.
fn finish(mut report: Report, strict: bool) -> TestReport {
    report.diagnostics.sort_by(|a, b| a.file.cmp(&b.file));
    report.summary.errors = count(&report, Severity::Error);
    report.summary.warnings = count(&report, Severity::Warning);
    TestReport { report, strict }
}

/// Test `entries` against a target, given a describer, its catalog and its `TimeZone` setting.
/// Describes run concurrently, at most `opts.pool` at a time.
pub async fn test_with<D: Describe>(
    entries: &[TestEntry],
    db: &D,
    cat: &Catalog,
    timezone: &str,
    opts: &TestOptions,
) -> TestReport {
    let mut report = Report::new("test");
    report.summary.queries = entries.len();
    if opts.timestamp.as_deref() == Some("utc") && !utc_timezone(timezone) {
        report.push(ReportedDiagnostic::project(
            "QF048",
            format!(
                "the database time zone is \"{timezone}\", but timestamps are configured as UTC (\"timestamp\": \"utc\")"
            ),
            Some(
                "set the time zone of the database or role to UTC, e.g. \
                 ALTER DATABASE <name> SET timezone TO 'UTC'"
                    .into(),
            ),
        ));
    }
    let mut stream = futures_util::stream::iter(
        entries
            .iter()
            .enumerate()
            .map(|(i, e)| async move { (i, test_entry(e, db, cat).await) }),
    )
    .buffer_unordered(opts.pool.max(1));
    let mut done = Vec::with_capacity(entries.len());
    while let Some((i, r)) = stream.next().await {
        match r {
            Ok(ds) => done.push((i, ds)),
            Err(e) => {
                // As generate: the connection error alone; the rest would be noise.
                let mut report = Report::new("test");
                report.summary.queries = entries.len();
                report.push(connection_diagnostic(&e));
                return finish(report, opts.strict);
            }
        }
    }
    done.sort_by_key(|(i, _)| *i);
    for (_, ds) in done {
        for d in ds {
            report.push(d);
        }
    }
    finish(report, opts.strict)
}

/// What `qf test` reads its metas from.
#[derive(Debug, Clone, Copy)]
pub enum Source<'a> {
    /// `*.qf.json` files in the project, narrowed by literal paths.
    Tree(&'a [PathBuf]),
    /// A manifest file; the paths (absolute, or relative to the project) narrow it by source.
    Manifest(&'a Path, &'a [PathBuf]),
}

fn fatal(d: ReportedDiagnostic, strict: bool) -> TestReport {
    let mut report = Report::new("test");
    report.push(d);
    finish(report, strict)
}

fn usage(message: String) -> ReportedDiagnostic {
    ReportedDiagnostic::project("QF091", message, None)
}

/// `source` is the project-relative path `rel`, or below it (everything is below `""`).
fn within(rel: &str, source: &str) -> bool {
    rel.is_empty() || source == rel || source.starts_with(&format!("{rel}/"))
}

/// The manifest's queries whose source is one of `paths` or below one (all without paths).
fn narrow(
    project: &Path,
    queries: Vec<QueryMeta>,
    paths: &[PathBuf],
) -> Result<Vec<QueryMeta>, ReportedDiagnostic> {
    if paths.is_empty() {
        return Ok(queries);
    }
    let mut scopes = Vec::new();
    for p in paths {
        let abs = if p.is_absolute() {
            p.clone()
        } else {
            project.join(p)
        };
        let rel = abs.strip_prefix(project).map_err(|_| {
            usage(format!(
                "{} is outside the project directory {}",
                p.display(),
                project.display()
            ))
        })?;
        let rel = posix(rel);
        if !queries.iter().any(|q| within(&rel, &q.source)) {
            return Err(usage(format!("no query in the manifest is in {rel}")));
        }
        scopes.push(rel);
    }
    Ok(queries
        .into_iter()
        .filter(|q| scopes.iter().any(|rel| within(rel, &q.source)))
        .collect())
}

/// Everything up to the report: config, metas, connection, the comparison.
pub fn test_report(
    project: &Path,
    source: Source<'_>,
    db: Option<&str>,
    strict: bool,
) -> TestReport {
    let cfg = match load_config(project) {
        Ok(c) => c,
        Err(d) => return fatal(ReportedDiagnostic::new(Some(CONFIG_FILE), &d), strict),
    };
    let (entries, timestamp) = match source {
        Source::Manifest(path, paths) => {
            let manifest = match std::fs::read_to_string(path)
                .map_err(|e| e.to_string())
                .and_then(|t| manifest_from_json(&t).map_err(|e| e.to_string()))
            {
                Ok(m) => m,
                Err(e) => {
                    let msg = format!("cannot read manifest {}: {e}", path.display());
                    let hint = "write it with `qf bundle`".to_string();
                    return fatal(
                        ReportedDiagnostic::project("QF091", msg, Some(hint)),
                        strict,
                    );
                }
            };
            let queries = match narrow(project, manifest.queries, paths) {
                Ok(q) => q,
                Err(d) => return fatal(d, strict),
            };
            let entries: Vec<TestEntry> = queries
                .into_iter()
                .map(|m| TestEntry {
                    file: m.source.clone(),
                    meta: Ok(m),
                })
                .collect();
            (
                entries,
                cfg.timestamp.clone().or(manifest.settings.timestamp),
            )
        }
        Source::Tree(paths) => {
            let rels = match discover_metas(project, &cfg, paths) {
                Ok(r) => r,
                Err(d) => {
                    return fatal(
                        ReportedDiagnostic::project(d.code, d.message, d.hint),
                        strict,
                    );
                }
            };
            let entries: Vec<TestEntry> = read_metas(project, &rels)
                .into_iter()
                .map(|(file, meta)| TestEntry { file, meta })
                .collect();
            (entries, cfg.timestamp.clone())
        }
    };
    let opts = TestOptions {
        strict,
        timestamp,
        pool: cfg.pool,
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return fatal(usage(format!("cannot start: {e}")), strict),
    };
    rt.block_on(async {
        let stop = |e: ConnError| {
            let mut r = fatal(connection_diagnostic(&e), strict);
            r.report.summary.queries = entries.len();
            r
        };
        let conn = match resolve_connection(&test_connection_sources(cfg.test.as_ref(), db)) {
            Ok(c) => c,
            Err(e) => return stop(e),
        };
        let size = cfg.pool.min(entries.len()).max(1);
        let pool = match connect_pool(&conn, size, CONNECT_TIMEOUT).await {
            Ok(p) => p,
            Err(e) => return stop(e),
        };
        let cat = match Catalog::load(&pool).await {
            Ok(c) => c,
            Err(e) => return stop(e),
        };
        test_with(&entries, &pool, &cat, cat.timezone(), &opts).await
    })
}

/// Run `qf test`, print the report (stderr, or JSON on stdout) and return the exit code.
pub fn run(project: &Path, source: Source<'_>, db: Option<&str>, strict: bool, json: bool) -> u8 {
    let r = test_report(project, source, db, strict);
    r.print(json);
    r.exit_code()
}
