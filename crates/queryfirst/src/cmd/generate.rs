//! `qf generate [paths…] [--db <conn>]`: describe the queries in scope against the dev database,
//! write their meta files and wrappers, and clean up after deleted queries.

use std::path::{Path, PathBuf};
use std::time::Duration;

use qf_pg::{
    Catalog, ConnError, ConnSource, Described, PgQueryError, ResolvedConn, connect_pool,
    resolve_connection,
};

use crate::config::{CONFIG_FILE, Config, load_config};
use crate::discover::discover;
use crate::engine::{Analyzed, Describe, analyze_all, apply, plan};
use crate::posix;
use crate::report::{
    Report, ReportedDiagnostic, print_report, render_diagnostic_list, summary_line, target_line,
};

/// How long opening the pool may take before the database counts as unreachable.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The report entry for a connection or connection-configuration error.
pub fn connection_diagnostic(e: &ConnError) -> ReportedDiagnostic {
    let hint = e.hint.clone().or_else(|| {
        (e.code == "QF090").then(|| {
            "is the database running, and the SSH tunnel open if there is one? \
         Agents: ask a human rather than retrying"
                .to_string()
        })
    });
    ReportedDiagnostic::project(e.code, e.to_string(), hint)
}

/// The connection sources for generate: `--db`, then the configured environment variable if
/// there is one, else `DATABASE_URL` / the `PG*` variables.
pub fn connection_sources(cfg: &Config, db: Option<&str>) -> Vec<ConnSource> {
    let mut sources = Vec::new();
    if let Some(db) = db {
        sources.push(ConnSource::Explicit(db.to_string()));
    }
    match &cfg.connection {
        Some(c) => sources.push(ConnSource::Env(c.env.clone())),
        None => sources.push(ConnSource::StandardEnv),
    }
    sources
}

/// The connection for commands that only read and may point at any database, production
/// included (`qf describe`, `qf schema`): [`connection_sources`], with every session read-only
/// (`default_transaction_read_only=on`), as for `qf test`.
pub fn read_only_connection(cfg: &Config, db: Option<&str>) -> Result<ResolvedConn, ConnError> {
    resolve_connection(&connection_sources(cfg, db)).map(ResolvedConn::read_only)
}

/// Used when there is nothing to describe, so no connection is opened.
struct NoDatabase;

impl Describe for NoDatabase {
    async fn describe(&self, _: &str) -> Result<Described, PgQueryError> {
        Err(PgQueryError::Connection(
            "no database connection was opened".into(),
        ))
    }
}

/// The report entries for one query's diagnostics.
fn query_diagnostics(a: &Analyzed) -> Vec<ReportedDiagnostic> {
    let file = posix(&a.source);
    a.diagnostics
        .iter()
        .map(|d| ReportedDiagnostic::new(Some(&file), d))
        .collect()
}

/// The generate pipeline for already discovered `files`, given a describer and catalog.
/// `whole_project` enables orphan cleanup.
pub async fn generate_with<D: Describe>(
    project: &Path,
    cfg: &Config,
    files: &[PathBuf],
    whole_project: bool,
    db: &D,
    cat: &Catalog,
) -> Report {
    generate_streaming(project, cfg, files, whole_project, db, cat, &mut |_| {}).await
}

/// [`generate_with`], calling `on_query` as each query is analyzed (before anything is written).
pub async fn generate_streaming<D: Describe>(
    project: &Path,
    cfg: &Config,
    files: &[PathBuf],
    whole_project: bool,
    db: &D,
    cat: &Catalog,
    on_query: &mut dyn FnMut(&Analyzed),
) -> Report {
    let mut report = Report::new("generate");
    report.summary.queries = files.len();
    match analyze_all(project, cfg, files, db, cat, on_query).await {
        Err(e) => report.push(connection_diagnostic(&e)),
        Ok(mut analyzed) => {
            let planned = plan(project, cfg, &mut analyzed, whole_project);
            // Query diagnostics are reported even when the plan fails.
            for a in &analyzed {
                for d in query_diagnostics(a) {
                    report.push(d);
                }
            }
            match planned.and_then(|p| apply(project, &p)) {
                Ok((written, deleted)) => {
                    report.written = written;
                    report.deleted = deleted;
                }
                Err(d) => report.push(d),
            }
        }
    }
    report.finish();
    report
}

fn fatal(mut report: Report, d: ReportedDiagnostic) -> Report {
    report.push(d);
    report.finish();
    report
}

/// Everything up to the report: config, discovery, connection, the pipeline. `on_query` as for
/// [`generate_streaming`].
pub fn generate_report(
    project: &Path,
    paths: &[PathBuf],
    db: Option<&str>,
    on_query: &mut dyn FnMut(&Analyzed),
) -> Report {
    let mut report = Report::new("generate");
    let cfg = match load_config(project) {
        Ok(c) => c,
        Err(d) => return fatal(report, ReportedDiagnostic::new(Some(CONFIG_FILE), &d)),
    };
    let files = match discover(project, &cfg, paths) {
        Ok(f) => f,
        Err(d) => {
            return fatal(
                report,
                ReportedDiagnostic::project(d.code, d.message, d.hint),
            );
        }
    };
    report.summary.queries = files.len();
    let whole_project = paths.is_empty();
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let d = ReportedDiagnostic::project("QF091", format!("cannot start: {e}"), None);
            return fatal(report, d);
        }
    };
    rt.block_on(async {
        if files.is_empty() {
            return generate_with(
                project,
                &cfg,
                &files,
                whole_project,
                &NoDatabase,
                &Catalog::default(),
            )
            .await;
        }
        let conn = match resolve_connection(&connection_sources(&cfg, db)) {
            Ok(c) => c,
            Err(e) => return fatal(report, connection_diagnostic(&e)),
        };
        report.target = Some(conn.to_string());
        let size = cfg.pool.min(files.len()).max(1);
        let pool = match connect_pool(&conn, size, CONNECT_TIMEOUT).await {
            Ok(p) => p,
            Err(e) => return fatal(report, connection_diagnostic(&e)),
        };
        let cat = match Catalog::load(&pool).await {
            Ok(c) => c,
            Err(e) => return fatal(report, connection_diagnostic(&e)),
        };
        let mut done =
            generate_streaming(project, &cfg, &files, whole_project, &pool, &cat, on_query).await;
        done.target = report.target;
        done
    })
}

/// Run `qf generate`, print the report and return the exit code. Without `json`, each query's
/// diagnostics are printed as soon as it is analyzed; the rest of the report follows at the end.
pub fn run(project: &Path, paths: &[PathBuf], db: Option<&str>, json: bool) -> u8 {
    if json {
        let report = generate_report(project, paths, db, &mut |_| {});
        print_report(&report, true);
        return report.exit_code();
    }
    let mut printed: Vec<ReportedDiagnostic> = Vec::new();
    let report = generate_report(project, paths, db, &mut |a| {
        let ds = query_diagnostics(a);
        anstream::eprint!("{}", render_diagnostic_list(&ds, true));
        printed.extend(ds);
    });
    // Whatever was not streamed: plan and write errors, generator errors, connection trouble.
    let rest: Vec<ReportedDiagnostic> = report
        .diagnostics
        .iter()
        .filter(|d| match printed.iter().position(|p| p == *d) {
            Some(i) => {
                printed.swap_remove(i);
                false
            }
            None => true,
        })
        .cloned()
        .collect();
    anstream::eprint!(
        "{}{}{}",
        render_diagnostic_list(&rest, true),
        target_line(report.target.as_deref()),
        summary_line(&report, true)
    );
    report.exit_code()
}
