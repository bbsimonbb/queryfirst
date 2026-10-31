//! `qf describe [file] [--sql <text>] [--db <conn>]`: run the generate pipeline for one query
//! and print what it found (params and columns with types, nullability and the reasons), writing
//! nothing and giving no verdict.

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

use qf_meta::{QueryMeta, Transform, TypeRef, to_canonical_json};
use qf_pg::{Catalog, ConnError, connect_pool, resolve_connection};
use qf_sqlfile::Severity;
use serde::Serialize;

use crate::cmd::generate::{CONNECT_TIMEOUT, connection_diagnostic, connection_sources};
use crate::config::{CONFIG_FILE, load_config};
use crate::engine::{Analyzed, Describe, analyze_path, analyze_text};
use crate::posix;
use crate::report::{Report, ReportedDiagnostic, print_report, render_diagnostics, render_human};

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
pub async fn describe_with<D: Describe>(
    project: &Path,
    input: &Input,
    db: &D,
    cat: &Catalog,
) -> Result<Analyzed, ConnError> {
    let mut a = match input {
        Input::Sql(text) => {
            analyze_text(
                VIRTUAL_SOURCE,
                Path::new(VIRTUAL_NAME_PATH),
                text,
                db,
                cat,
                None,
            )
            .await?
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

/// What `describe` found: the analysed query, or a report of what stopped it.
enum Outcome {
    Described(Box<Analyzed>),
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
        let stop = |e: ConnError| {
            Outcome::Stopped(fatal(Report::new("describe"), connection_diagnostic(&e)))
        };
        let conn = match resolve_connection(&connection_sources(&cfg, db)) {
            Ok(c) => c,
            Err(e) => return stop(e),
        };
        let pool = match connect_pool(&conn, 1, CONNECT_TIMEOUT).await {
            Ok(p) => p,
            Err(e) => return stop(e),
        };
        let cat = match Catalog::load(&pool).await {
            Ok(c) => c,
            Err(e) => return stop(e),
        };
        match describe_with(project, input, &pool, &cat).await {
            Ok(a) => Outcome::Described(Box::new(a)),
            Err(e) => stop(e),
        }
    })
}

/// Run `qf describe`, print the result and return the exit code.
///
/// The description goes to stdout (text, or with `json` the canonical meta); diagnostics go to
/// stderr, except that notes and warnings join the text description on stdout.
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
    let a = match describe_input(project, &input, db) {
        Outcome::Described(a) => a,
        Outcome::Stopped(report) => {
            print_report(&report, json);
            return report.exit_code();
        }
    };
    let mut report = Report::new("describe");
    report.summary.queries = 1;
    let file = posix(&a.source);
    for d in &a.diagnostics {
        report.push(ReportedDiagnostic::new(Some(&file), d));
    }
    report.finish();
    let broken = a.diagnostics.iter().any(|d| d.severity == Severity::Error);
    if json {
        println!("{}", to_canonical_json(&a.meta));
        eprint!("{}", render_diagnostics(&report));
    } else if broken {
        eprint!("{}", render_human(&report));
    } else {
        print!("{}{}", render_text(&a.meta), render_diagnostics(&report));
    }
    report.exit_code()
}
