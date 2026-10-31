//! `qf bundle [-o file]`: write every meta file of the project into one
//! `queryfirst.manifest.json`, which ships in the deployment image so `qf test --manifest` can
//! run there without the source tree (spec §9a).
//!
//! The manifest is canonical (sorted by source, LF, trailing newline), so two runs over the same
//! metas give the same bytes. It carries the `timestamp` setting for the time zone check. A meta
//! that cannot be read is `QF030` (as in `qf check`) and nothing is written. Broken queries are
//! bundled as they are: `qf test` reports them (`QF049`).

use std::path::Path;

use qf_meta::{MANIFEST_FILE, Manifest, ManifestSettings, QueryMeta, from_json, manifest_to_json};
use qf_sqlfile::Severity;

use crate::config::{CONFIG_FILE, load_config};
use crate::discover::discover_metas;
use crate::posix;
use crate::report::{Report, ReportedDiagnostic, print_report};
use crate::writer::write_if_changed;

/// Read each project-relative meta file: the meta, or why it cannot be read.
pub fn read_metas(project: &Path, rels: &[String]) -> Vec<(String, Result<QueryMeta, String>)> {
    rels.iter()
        .map(|rel| {
            let meta = std::fs::read_to_string(project.join(rel))
                .map_err(|e| e.to_string())
                .and_then(|t| from_json(&t).map_err(|e| e.to_string()));
            (rel.clone(), meta)
        })
        .collect()
}

fn fatal(mut report: Report, d: ReportedDiagnostic) -> Report {
    report.push(d);
    report.finish();
    report
}

/// Everything up to the report. `output` defaults to `queryfirst.manifest.json` at the project
/// root; a relative `output` is relative to the project.
pub fn bundle_report(project: &Path, output: Option<&Path>) -> Report {
    let mut report = Report::new("bundle");
    let cfg = match load_config(project) {
        Ok(c) => c,
        Err(d) => return fatal(report, ReportedDiagnostic::new(Some(CONFIG_FILE), &d)),
    };
    let rels = match discover_metas(project, &cfg, &[]) {
        Ok(r) => r,
        Err(d) => {
            return fatal(
                report,
                ReportedDiagnostic::project(d.code, d.message, d.hint),
            );
        }
    };
    report.summary.queries = rels.len();
    let mut queries = Vec::new();
    for (rel, meta) in read_metas(project, &rels) {
        match meta {
            Ok(m) => queries.push(m),
            Err(e) => report.push(ReportedDiagnostic {
                file: Some(rel.clone()),
                line: 0,
                col: 0,
                code: "QF030".into(),
                severity: Severity::Error,
                message: format!("{rel} cannot be read: {e}"),
                hint: Some("run `qf generate` and commit the result".into()),
            }),
        }
    }
    if report.ok() {
        let manifest = Manifest::new(
            ManifestSettings {
                timestamp: cfg.timestamp.clone(),
            },
            queries,
        );
        let path = match output {
            Some(o) => project.join(o),
            None => project.join(MANIFEST_FILE),
        };
        let shown = path
            .strip_prefix(project)
            .map_or_else(|_| path.display().to_string(), posix);
        match write_if_changed(&path, &manifest_to_json(&manifest)) {
            Ok(true) => report.written.push(shown),
            Ok(false) => {}
            Err(e) => report.push(ReportedDiagnostic::project(
                "QF091",
                format!("cannot write {shown}: {e}"),
                None,
            )),
        }
    }
    report.finish();
    report
}

/// Run `qf bundle`, print the report and return the exit code.
pub fn run(project: &Path, output: Option<&Path>, json: bool) -> u8 {
    let report = bundle_report(project, output);
    print_report(&report, json);
    report.exit_code()
}
