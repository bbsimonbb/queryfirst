//! `qf describe [file] [--sql <text>] [--db <conn>]`: run the generate pipeline for one query
//! and print what it found (params and columns with types, nullability and the reasons), writing
//! nothing and giving no verdict.

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

use qf_meta::{QueryMeta, Transform, TypeRef, to_canonical_json};
use qf_pg::{Catalog, ConnError, connect_pool};
use qf_sqlfile::Severity;
use serde::Serialize;

use crate::cmd::generate::{CONNECT_TIMEOUT, connection_diagnostic, read_only_connection};
use crate::config::{CONFIG_FILE, load_config};
use crate::engine::{Analyzed, Describe, analyze_path, analyze_text};
use crate::posix;
use crate::report::{
    Report, ReportedDiagnostic, print_report, render_diagnostics, render_human, target_line,
};

/// The source name of a query given as text.
const VIRTUAL_SOURCE: &str = "<sql>";
/// Its default name, unless an `@name` header says otherwise.
const VIRTUAL_NAME_PATH: &str = "adhoc.sql";

/// The one query to describe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// A query file, as a path usable from the current directory.
    File(PathBuf),
    /// The body of the virtual file `<sql>`.
    Sql(String),
}

fn usage(message: &str) -> ReportedDiagnostic {
    ReportedDiagnostic::project(
        "QF091",
        message.to_string(),
        Some("usage: qf describe [file] [--sql <text>] [--db <conn>] [--json]".into()),
    )
}

/// Pick the single input: a file, `--sql`, or else piped stdin. `stdin` is called only when
/// neither is given, and returns `None` when stdin is a terminal.
pub fn choose_input(
    file: Option<&Path>,
    sql: Option<&str>,
    stdin: impl FnOnce() -> Option<String>,
) -> Result<Input, ReportedDiagnostic> {
    match (file, sql) {
        (Some(_), Some(_)) => Err(usage(
            "give exactly one of a file or --sql (or pipe the query on stdin)",
        )),
        (Some(f), None) => Ok(Input::File(f.to_path_buf())),
        (None, Some(s)) => Ok(Input::Sql(s.to_string())),
        (None, None) => match stdin() {
            Some(text) if !text.trim().is_empty() => Ok(Input::Sql(text)),
            _ => Err(usage(
                "no query given: pass a file, --sql <text>, or pipe the query on stdin",
            )),
        },
    }
}

/// The text on stdin, unless stdin is a terminal (or unreadable).
fn read_stdin() -> Option<String> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return None;
    }
    let mut text = String::new();
    stdin.lock().read_to_string(&mut text).ok()?;
    Some(text)
}

/// The shared pipeline for one query, given a describer and catalog. Only a connection error
/// is an `Err`; query problems are in [`Analyzed::diagnostics`]. Nothing is written.
///
/// Query text has no file name for an `@name` to disagree with, so it gets no `QF003`.
pub async fn describe_with<D: Describe>(
    project: &Path,
    input: &Input,
    db: &D,
    cat: &Catalog,
) -> Result<Analyzed, ConnError> {
    let mut a = match input {
        Input::Sql(text) => {
            let mut a = analyze_text(
                VIRTUAL_SOURCE,
                Path::new(VIRTUAL_NAME_PATH),
                text,
                db,
                cat,
                None,
            )
            .await?;
            a.diagnostics.retain(|d| d.code != "QF003");
            a
        }
        Input::File(path) => {
            let rel = path.strip_prefix(project).unwrap_or(path);
            analyze_path(path, &posix(rel), rel, db, cat, None).await?
        }
    };
    a.meta.generated.clear();
    Ok(a)
}

fn type_text(t: &TypeRef) -> String {
    format!(
        "{}{}{}",
        t.name,
        t.modifier.as_deref().unwrap_or(""),
        if t.array { "[]" } else { "" }
    )
}

/// The serialised name of a reason enum, e.g. `outer-join`.
fn reason_text<T: Serialize>(reason: &T) -> String {
    match serde_json::to_value(reason) {
        Ok(serde_json::Value::String(s)) => s,
        _ => String::new(),
    }
}

/// `rows` as lines indented by two spaces, with every field but the last padded to align.
fn push_rows(out: &mut String, rows: &[Vec<String>]) {
    let width = |i: usize| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0);
    let (w0, w1) = (width(0), width(1));
    for r in rows {
        out.push_str(&format!("  {:<w0$}  {:<w1$}  {}\n", r[0], r[1], r[2]));
    }
}

/// The human-readable description of a query's meta.
pub fn render_text(meta: &QueryMeta) -> String {
    let mut out = format!("{}\n", meta.name);
    if meta.params.is_empty() {
        out.push_str("params: none\n");
    } else {
        out.push_str("params:\n");
        let rows: Vec<Vec<String>> = meta
            .params
            .iter()
            .map(|p| {
                let mut state = format!(
                    "{} ({})",
                    if p.required { "required" } else { "optional" },
                    reason_text(&p.required_reason)
                );
                match p.transform {
                    Transform::Scalar => {}
                    Transform::Spread => state.push_str("  [spread]"),
                    Transform::Pick { .. } => state.push_str("  [pick]"),
                    Transform::PickSpread { .. } => state.push_str("  [pick spread]"),
                }
                vec![format!(":{}", p.name), type_text(&p.db_type), state]
            })
            .collect();
        push_rows(&mut out, &rows);
    }
    if meta.columns.is_empty() {
        out.push_str("columns: none\n");
    } else {
        out.push_str("columns:\n");
        let rows: Vec<Vec<String>> = meta
            .columns
            .iter()
            .map(|c| {
                vec![
                    c.name.clone(),
                    type_text(&c.db_type),
                    format!(
                        "{} ({})",
                        if c.nullable { "nullable" } else { "not null" },
                        reason_text(&c.nullable_reason)
                    ),
                ]
            })
            .collect();
        push_rows(&mut out, &rows);
    }
    out
}

fn fatal(mut report: Report, d: ReportedDiagnostic) -> Report {
    report.push(d);
    report.finish();
    report
}

/// What `describe` found: the analysed query and the database it used (`host:port/db`), or a
/// report of what stopped it.
enum Outcome {
    Described(Box<Analyzed>, String),
    Stopped(Report),
}

fn describe_input(project: &Path, input: &Input, db: Option<&str>) -> Outcome {
    let report = Report::new("describe");
    let cfg = match load_config(project) {
        Ok(c) => c,
        Err(d) => {
            return Outcome::Stopped(fatal(
                report,
                ReportedDiagnostic::new(Some(CONFIG_FILE), &d),
            ));
        }
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let d = ReportedDiagnostic::project("QF091", format!("cannot start: {e}"), None);
            return Outcome::Stopped(fatal(report, d));
        }
    };
    rt.block_on(async {
        let stop = |e: ConnError, target: Option<&str>| {
            let mut r = Report::new("describe");
            r.target = target.map(str::to_string);
            Outcome::Stopped(fatal(r, connection_diagnostic(&e)))
        };
        // Read-only sessions: describe may point at any database, production included.
        let conn = match read_only_connection(&cfg, db) {
            Ok(c) => c,
            Err(e) => return stop(e, None),
        };
        let target = conn.to_string();
        let stop = |e: ConnError| stop(e, Some(&target));
        let pool = match connect_pool(&conn, 1, CONNECT_TIMEOUT).await {
            Ok(p) => p,
            Err(e) => return stop(e),
        };
        let cat = match Catalog::load(&pool).await {
            Ok(c) => c,
            Err(e) => return stop(e),
        };
        match describe_with(project, input, &pool, &cat).await {
            Ok(a) => Outcome::Described(Box::new(a), target.clone()),
            Err(e) => stop(e),
        }
    })
}

/// Run `qf describe`, print the result and return the exit code.
///
/// stdout carries only the description (text, or with `json` the canonical meta); every
/// diagnostic, note and warning goes to stderr.
pub fn run(
    project: &Path,
    file: Option<&Path>,
    sql: Option<&str>,
    db: Option<&str>,
    json: bool,
) -> u8 {
    let input = match choose_input(file, sql, read_stdin) {
        Ok(i) => i,
        Err(d) => {
            let report = fatal(Report::new("describe"), d);
            print_report(&report, json);
            return report.exit_code();
        }
    };
    let (a, target) = match describe_input(project, &input, db) {
        Outcome::Described(a, target) => (a, target),
        Outcome::Stopped(report) => {
            print_report(&report, json);
            return report.exit_code();
        }
    };
    let (stdout, stderr, code) = render_outcome(&a, Some(&target), json);
    print!("{stdout}");
    eprint!("{stderr}");
    code
}

/// What to print for a described query: `(stdout, stderr, exit code)`. `target` is the database
/// it was described against (`host:port/db`), named on stderr in the human-readable output.
pub fn render_outcome(a: &Analyzed, target: Option<&str>, json: bool) -> (String, String, u8) {
    let mut report = Report::new("describe");
    report.summary.queries = 1;
    report.target = target.map(str::to_string);
    let file = posix(&a.source);
    for d in &a.diagnostics {
        report.push(ReportedDiagnostic::new(Some(&file), d));
    }
    report.finish();
    let broken = a.diagnostics.iter().any(|d| d.severity == Severity::Error);
    let (stdout, stderr) = if broken && !json {
        (String::new(), render_human(&report))
    } else if json {
        // The canonical JSON already ends with a newline.
        (to_canonical_json(&a.meta), render_diagnostics(&report))
    } else {
        let stderr = render_diagnostics(&report) + &target_line(report.target.as_deref());
        (render_text(&a.meta), stderr)
    };
    (stdout, stderr, report.exit_code())
}
