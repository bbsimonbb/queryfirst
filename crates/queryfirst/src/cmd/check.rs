//! `qf check [paths…]`: the offline CI step. It never connects to a database and reads no
//! connection settings; it compares hashes of what is committed.
//!
//! * `QF034` a query file has no meta file;
//! * `QF030` the query file changed since its meta was written (`sqlHash` differs);
//! * `QF033` the meta records a broken query. `generate` stores the `sqlHash` of the source as
//!   it was in the broken run, so an unchanged broken query reports only `QF033`, and one edited
//!   since reports both;
//! * `QF031` a generated file is missing or its hash differs from the recorded one (edited);
//! * `QF032` a generated file is unedited but regenerating from the meta gives other bytes (it
//!   was made by a different `qf` version or configuration). Wrappers, broken ones included,
//!   are regenerated with `generate(meta, None, …)`, exactly as `generate` rendered them;
//! * `QF035` a meta file whose query file is gone (orphan). Found by scanning for `*.qf.json`
//!   within the include/exclude scope: the whole project, or only the path arguments.
//!
//! An unreadable or unparsable meta is reported as `QF030` (regenerate it).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use qf_gen_ts::{PgtypedBinding, generate, output_path};
use qf_meta::{QueryMeta, Status, from_json, meta_path_for, sha256_tag};
use qf_sqlfile::Severity;

use crate::config::{CONFIG_FILE, Config, load_config};
use crate::discover::{Matcher, discover, walk};
use crate::posix;
use crate::report::{Report, ReportedDiagnostic, print_report};
use crate::writer::normalise_text;

const META_SUFFIX: &str = ".qf.json";
const REGENERATE: &str = "run `qf generate` and commit the result";

fn diag(file: &str, code: &str, message: String, hint: &str) -> ReportedDiagnostic {
    ReportedDiagnostic {
        file: Some(file.to_string()),
        line: 0,
        col: 0,
        code: code.to_string(),
        severity: Severity::Error,
        message,
        hint: Some(hint.to_string()),
    }
}

fn fatal(mut report: Report, d: ReportedDiagnostic) -> Report {
    report.push(d);
    report.finish();
    report
}

/// Every generator's output for `meta`, by project-relative path (normalised text, or the
/// generator's error).
fn regenerate(meta: &QueryMeta, cfg: &Config) -> BTreeMap<String, Result<String, String>> {
    cfg.generators
        .iter()
        .map(|g| {
            let opts = g.ts_options();
            match generate(meta, None, &opts, &PgtypedBinding) {
                Ok(f) => (f.path, Ok(normalise_text(&f.content))),
                Err(ds) => {
                    let msg = ds
                        .iter()
                        .map(|d| format!("{}: {}", d.code, d.message))
                        .collect::<Vec<_>>()
                        .join("; ");
                    (output_path(&meta.source, &opts), Err(msg))
                }
            }
        })
        .collect()
}

/// Check one query whose meta exists, pushing diagnostics.
fn check_query(project: &Path, cfg: &Config, source: &str, meta_rel: &str, report: &mut Report) {
    let meta = match std::fs::read_to_string(project.join(meta_rel))
        .map_err(|e| e.to_string())
        .and_then(|t| from_json(&t).map_err(|e| e.to_string()))
    {
        Ok(m) => m,
        Err(e) => {
            let msg = format!("{meta_rel} cannot be read: {e}");
            report.push(diag(source, "QF030", msg, REGENERATE));
            return;
        }
    };
    match std::fs::read_to_string(project.join(source)) {
        Ok(text) => {
            let hash = sha256_tag(qf_sqlfile::normalise(&text).as_bytes());
            if hash != meta.sql_hash {
                let msg = format!("{source} changed since {meta_rel} was generated");
                report.push(diag(source, "QF030", msg, REGENERATE));
            }
        }
        Err(e) => {
            let msg = format!("cannot read {source}: {e}");
            report.push(diag(source, "QF030", msg, REGENERATE));
        }
    }
    if let Status::Broken { diagnostics } = &meta.status {
        let first = diagnostics.first();
        let detail = first.map_or(String::new(), |d| format!(": {} {}", d.code, d.message));
        report.push(ReportedDiagnostic {
            file: Some(source.to_string()),
            line: first.map_or(0, |d| d.line),
            col: first.map_or(0, |d| d.col),
            code: "QF033".into(),
            severity: Severity::Error,
            message: format!("{meta_rel} records a broken query{detail}"),
            hint: Some("fix the query, then run `qf generate`".into()),
        });
    }
    let regenerated = regenerate(&meta, cfg);
    for g in &meta.generated {
        let on_disk = match std::fs::read_to_string(project.join(&g.path)) {
            Ok(t) => normalise_text(&t),
            Err(_) => {
                let msg = format!("generated file {} is missing", g.path);
                report.push(diag(&g.path, "QF031", msg, REGENERATE));
                continue;
            }
        };
        if sha256_tag(on_disk.as_bytes()) != g.hash {
            let msg = format!("{} does not match {meta_rel}: edited by hand?", g.path);
            report.push(diag(&g.path, "QF031", msg, REGENERATE));
            continue;
        }
        match regenerated.get(&g.path) {
            Some(Ok(expected)) if *expected == on_disk => {}
            Some(Err(e)) => {
                let msg = format!("{} cannot be regenerated from {meta_rel}: {e}", g.path);
                report.push(diag(&g.path, "QF032", msg, REGENERATE));
            }
            _ => {
                let msg = format!(
                    "{} differs from what this version of qf generates from {meta_rel}",
                    g.path
                );
                report.push(diag(&g.path, "QF032", msg, REGENERATE));
            }
        }
    }
}

/// The directories the orphan scan walks, project-relative: the project root, or the
/// directories named on the command line (file arguments have their own meta checked already).
fn orphan_scopes(root: &Path, paths: &[PathBuf]) -> Vec<String> {
    if paths.is_empty() {
        return vec![String::new()];
    }
    let mut scopes = Vec::new();
    for p in paths {
        let abs = if p.is_absolute() {
            p.clone()
        } else {
            root.join(p)
        };
        if let Ok(abs) = std::fs::canonicalize(abs)
            && abs.is_dir()
            && let Ok(rel) = abs.strip_prefix(root)
        {
            scopes.push(posix(rel));
        }
    }
    scopes
}

fn orphans(
    project: &Path,
    cfg: &Config,
    paths: &[PathBuf],
    seen: &BTreeSet<String>,
    report: &mut Report,
) {
    let Ok(root) = std::fs::canonicalize(project) else {
        return;
    };
    let m = Matcher::new(cfg);
    let mut found = BTreeSet::new();
    for scope in orphan_scopes(&root, paths) {
        let _ = walk(&root, &scope, &m, &mut |f| {
            if f.ends_with(META_SUFFIX) && !m.excluded(f) && !seen.contains(f) {
                found.insert(f.to_string());
            }
        });
    }
    for meta_rel in found {
        let stem = &meta_rel[..meta_rel.len() - META_SUFFIX.len()];
        let source = std::fs::read_to_string(root.join(&meta_rel))
            .ok()
            .and_then(|t| from_json(&t).ok())
            .map(|m| m.source);
        let exists = match source {
            Some(s) => root.join(s).is_file(),
            None => ["psql", "sql"]
                .iter()
                .any(|e| root.join(format!("{stem}.{e}")).is_file()),
        };
        if !exists {
            let msg = format!("{meta_rel} has no query file");
            let hint =
                "delete it and its generated files, or run `qf generate` on the whole project";
            report.push(diag(&meta_rel, "QF035", msg, hint));
        }
    }
}

/// Everything up to the report. Never touches the network.
pub fn check_report(project: &Path, paths: &[PathBuf]) -> Report {
    let mut report = Report::new("check");
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
    let mut seen = BTreeSet::new();
    for f in &files {
        let source = posix(f);
        let meta_rel = posix(&meta_path_for(f));
        if project.join(&meta_rel).is_file() {
            check_query(project, &cfg, &source, &meta_rel, &mut report);
        } else {
            let msg = format!("{source} has no meta file ({meta_rel})");
            report.push(diag(&source, "QF034", msg, REGENERATE));
        }
        seen.insert(meta_rel);
    }
    orphans(project, &cfg, paths, &seen, &mut report);
    report.finish();
    report
}

/// Run `qf check`, print the report and return the exit code.
pub fn run(project: &Path, paths: &[PathBuf], json: bool) -> u8 {
    let report = check_report(project, paths);
    print_report(&report, json);
    report.exit_code()
}
