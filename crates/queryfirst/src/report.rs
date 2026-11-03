//! What a command reports: diagnostics, written and deleted files, a summary. Human-readable
//! text goes to stderr; with `--json`, one JSON document goes to stdout instead.

use qf_sqlfile::{Diagnostic, Severity};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportedDiagnostic {
    /// Project-relative POSIX path; `None` for project-wide problems (connection, config).
    pub file: Option<String>,
    pub line: u32,
    pub col: u32,
    pub code: String,
    pub severity: Severity,
    pub message: String,
    pub hint: Option<String>,
}

impl ReportedDiagnostic {
    pub fn new(file: Option<&str>, d: &Diagnostic) -> Self {
        ReportedDiagnostic {
            file: file.map(str::to_string),
            line: d.line,
            col: d.col,
            code: d.code.to_string(),
            severity: d.severity,
            message: d.message.clone(),
            hint: d.hint.clone(),
        }
    }

    /// A project-wide error with no position.
    pub fn project(code: &str, message: String, hint: Option<String>) -> Self {
        ReportedDiagnostic {
            file: None,
            line: 0,
            col: 0,
            code: code.to_string(),
            severity: Severity::Error,
            message,
            hint,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub queries: usize,
    pub errors: usize,
    pub warnings: usize,
    pub written: usize,
    pub deleted: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub command: String,
    pub diagnostics: Vec<ReportedDiagnostic>,
    pub written: Vec<String>,
    pub deleted: Vec<String>,
    pub summary: Summary,
    /// The database the command used, as `host:port/db` (never a user or password), once a
    /// connection was resolved. Shown in the human-readable output only.
    pub target: Option<String>,
}

/// Codes that mean usage, configuration or connection trouble: exit 2.
const FATAL: [&str; 2] = ["QF090", "QF091"];

impl Report {
    pub fn new(command: &str) -> Self {
        Report {
            command: command.to_string(),
            diagnostics: Vec::new(),
            written: Vec::new(),
            deleted: Vec::new(),
            summary: Summary::default(),
            target: None,
        }
    }

    pub fn push(&mut self, d: ReportedDiagnostic) {
        self.diagnostics.push(d);
    }

    /// Sort everything and recompute the summary counts (`queries` is set by the command).
    pub fn finish(&mut self) {
        self.diagnostics.sort_by(|a, b| {
            (&a.file, a.line, a.col, &a.code, &a.message)
                .cmp(&(&b.file, b.line, b.col, &b.code, &b.message))
        });
        self.written.sort();
        self.deleted.sort();
        let count = |s: Severity| self.diagnostics.iter().filter(|d| d.severity == s).count();
        self.summary.errors = count(Severity::Error);
        self.summary.warnings = count(Severity::Warning);
        self.summary.written = self.written.len();
        self.summary.deleted = self.deleted.len();
    }

    pub fn ok(&self) -> bool {
        !self
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    /// 0 ok; 1 query errors; 2 usage, configuration or connection errors. Warnings and notes
    /// never change it.
    pub fn exit_code(&self) -> u8 {
        let errors = self
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error);
        let mut code = 0;
        for d in errors {
            if FATAL.contains(&d.code.as_str()) {
                return 2;
            }
            code = 1;
        }
        code
    }
}

fn severity_str(s: Severity) -> &'static str {
    match s {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Note => "note",
    }
}

pub(crate) fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Errors, and the error count in the summary line when it is not zero.
const ERROR_STYLE: anstyle::Style = anstyle::Style::new()
    .bold()
    .fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Red)));

/// `text`, in `style` when `color`.
fn paint(text: &str, style: anstyle::Style, color: bool) -> String {
    if color {
        format!("{style}{text}{style:#}")
    } else {
        text.to_string()
    }
}

/// `path:line:col: error[QFnnn]: message`, then `  hint: …`, then the [`target_line`] and a
/// summary line.
pub fn render_human(r: &Report) -> String {
    render_human_styled(r, false)
}

/// [`render_human`], with errors in red when `color`.
pub fn render_human_styled(r: &Report, color: bool) -> String {
    let mut out = render_diagnostic_list(&r.diagnostics, color);
    out.push_str(&target_line(r.target.as_deref()));
    out.push_str(&summary_line(r, color));
    out
}

/// `Queryfirst <command>: 3 queries, 1 error, 0 warnings; 2 files written, 0 deleted` and a
/// newline; the error count is red when `color` and there are errors.
pub fn summary_line(r: &Report, color: bool) -> String {
    let s = &r.summary;
    let errors = plural(s.errors, "error", "errors");
    let errors = paint(&errors, ERROR_STYLE, color && s.errors > 0);
    format!(
        "Queryfirst {}: {}, {}, {}; {} written, {} deleted\n",
        r.command,
        plural(s.queries, "query", "queries"),
        errors,
        plural(s.warnings, "warning", "warnings"),
        plural(s.written, "file", "files"),
        s.deleted
    )
}

/// `database: host:port/db` and a newline, so a run against the wrong database is noticed; empty
/// when no connection was resolved.
pub fn target_line(target: Option<&str>) -> String {
    target.map_or_else(String::new, |t| format!("database: {t}\n"))
}

/// Just the diagnostics of [`render_human`], without the summary line.
pub fn render_diagnostics(r: &Report) -> String {
    render_diagnostic_list(&r.diagnostics, false)
}

/// The lines of [`render_diagnostics`] for `diagnostics`, error lines in red when `color`.
pub fn render_diagnostic_list(diagnostics: &[ReportedDiagnostic], color: bool) -> String {
    let mut out = String::new();
    for d in diagnostics {
        let sev = severity_str(d.severity);
        let mut line = match &d.file {
            Some(f) if d.line > 0 => format!("{f}:{}:{}: ", d.line, d.col),
            Some(f) => format!("{f}: "),
            None => String::new(),
        };
        line.push_str(&format!("{sev}[{}]: {}", d.code, d.message));
        let is_error = d.severity == Severity::Error;
        out.push_str(&paint(&line, ERROR_STYLE, color && is_error));
        out.push('\n');
        if let Some(h) = &d.hint {
            out.push_str(&format!("  hint: {}\n", h.replace('\n', "\n        ")));
        }
    }
    out
}

/// `{"command","ok","summary":{…},"diagnostics":[…],"written":[],"deleted":[]}`.
pub fn to_json(r: &Report) -> Value {
    let s = &r.summary;
    json!({
        "command": r.command,
        "ok": r.ok(),
        "summary": {
            "queries": s.queries,
            "errors": s.errors,
            "warnings": s.warnings,
            "written": s.written,
            "deleted": s.deleted,
        },
        "diagnostics": r.diagnostics.iter().map(|d| json!({
            "file": d.file,
            "line": d.line,
            "col": d.col,
            "code": d.code,
            "severity": severity_str(d.severity),
            "message": d.message,
            "hint": d.hint,
        })).collect::<Vec<_>>(),
        "written": r.written,
        "deleted": r.deleted,
    })
}

/// Print the report: JSON on stdout with `json`, else human-readable text on stderr (in colour
/// when stderr is a terminal and `NO_COLOR` is not set).
pub fn print_report(r: &Report, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&to_json(r)).expect("report serialises")
        );
    } else {
        anstream::eprint!("{}", render_human_styled(r, true));
    }
}
