//! `qf init [--from-pgtyped]`: write a `queryfirst.json`.
//!
//! Plain `init` writes the defaults. With `--from-pgtyped` the settings come from the project's
//! `pgtyped-config.json`, or else (a tolerant literal scan) from `.pgtyped.ts`. Settings with no
//! counterpart are dropped with a note, and so is anything that could not be read. An existing
//! `queryfirst.json` is never overwritten (`QF091`, exit 2).

use std::collections::BTreeMap;
use std::path::Path;

use qf_sqlfile::Severity;
use serde::Serialize;
use serde_json::Value;

use crate::config::{CONFIG_FILE, Config, DEFAULT_EMIT_TEMPLATE, parse_config};
use crate::report::{Report, ReportedDiagnostic, render_diagnostics, to_json};
use crate::writer::write_if_changed;

const SCHEMA_REF: &str = "./node_modules/queryfirst/queryfirst.schema.json";
const PGTYPED_JSON: &str = "pgtyped-config.json";
const PGTYPED_TS: &str = ".pgtyped.ts";
/// Code of the notes `--from-pgtyped` prints (not an error; never changes the exit code).
const NOTE_CODE: &str = "QF092";

/// The file as written. Field order is the order in the file; only needed keys are present.
#[derive(Serialize)]
struct OutConfig {
    #[serde(rename = "$schema")]
    schema: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    include: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exclude: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    generators: Vec<OutGenerator>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OutGenerator {
    name: &'static str,
    emit_template: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    types_overrides: BTreeMap<String, Value>,
    /// Written only when true (the default is false).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    camel_case_column_names: bool,
}

fn note(message: String) -> ReportedDiagnostic {
    ReportedDiagnostic {
        file: None,
        line: 0,
        col: 0,
        code: NOTE_CODE.into(),
        severity: Severity::Note,
        message,
        hint: None,
    }
}

fn fail(mut report: Report, message: String, hint: Option<&str>) -> Report {
    report.push(ReportedDiagnostic::project(
        "QF091",
        message,
        hint.map(str::to_string),
    ));
    report.finish();
    report
}

/// Everything up to printing. Writes `queryfirst.json` into `project` unless it exists.
pub fn init_report(project: &Path, from_pgtyped: bool) -> Report {
    let mut report = Report::new("init");
    if project.join(CONFIG_FILE).exists() {
        return fail(
            report,
            format!("{CONFIG_FILE} already exists; refusing to overwrite it"),
            Some("delete it first, or edit it by hand"),
        );
    }
    let out = if from_pgtyped {
        match convert_pgtyped(project, &mut report) {
            Ok(o) => o,
            Err(m) => {
                return fail(
                    report,
                    m,
                    Some("expected pgtyped-config.json or .pgtyped.ts in the project directory"),
                );
            }
        }
    } else {
        let d = Config::default();
        OutConfig {
            schema: SCHEMA_REF,
            include: Some(d.include),
            exclude: Some(d.exclude),
            generators: Vec::new(),
        }
    };
    let text = serde_json::to_string_pretty(&out).expect("config serialises") + "\n";
    // What is written must load.
    if let Err(d) = parse_config(&text) {
        return fail(
            report,
            format!("the converted configuration is invalid: {}", d.message),
            None,
        );
    }
    match write_if_changed(&project.join(CONFIG_FILE), &text) {
        Ok(_) => report.written.push(CONFIG_FILE.into()),
        Err(e) => return fail(report, format!("cannot write {CONFIG_FILE}: {e}"), None),
    }
    report.finish();
    report
}

/// Run `qf init`, print the report and return the exit code.
pub fn run(project: &Path, from_pgtyped: bool, json: bool) -> u8 {
    let report = init_report(project, from_pgtyped);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&to_json(&report)).expect("report serialises")
        );
    } else {
        eprint!("{}", render_diagnostics(&report));
        if report.ok() {
            eprintln!("Queryfirst init: wrote {CONFIG_FILE}");
        }
    }
    report.exit_code()
}

fn convert_pgtyped(project: &Path, report: &mut Report) -> Result<OutConfig, String> {
    let json_path = project.join(PGTYPED_JSON);
    let ts_path = project.join(PGTYPED_TS);
    let (value, unreadable) = if json_path.is_file() {
        let text = std::fs::read_to_string(&json_path)
            .map_err(|e| format!("cannot read {PGTYPED_JSON}: {e}"))?;
        let v = serde_json::from_str::<Value>(&text)
            .map_err(|e| format!("{PGTYPED_JSON} is not valid JSON: {e}"))?;
        (v, Vec::new())
    } else if ts_path.is_file() {
        let text = std::fs::read_to_string(&ts_path)
            .map_err(|e| format!("cannot read {PGTYPED_TS}: {e}"))?;
        scan_ts(&text).ok_or_else(|| format!("found no configuration object in {PGTYPED_TS}"))?
    } else {
        return Err(format!("found neither {PGTYPED_JSON} nor {PGTYPED_TS}"));
    };
    let Some(obj) = value.as_object() else {
        return Err("the pgtyped configuration is not an object".into());
    };

    // Expressions under `db` (process.env.PGHOST ...) are expected, and `db` is dropped anyway.
    for path in unreadable
        .iter()
        .filter(|p| p.as_str() != "db" && !p.starts_with("db."))
    {
        report.push(note(format!(
            "could not read `{path}` (not a literal value); it was left out"
        )));
    }

    // srcDir + transforms[mode=sql].include -> include
    let src_dir = obj
        .get("srcDir")
        .and_then(Value::as_str)
        .map(|s| s.trim_start_matches("./").trim_matches('/').to_string())
        .filter(|s| !s.is_empty() && s != ".");
    let mut include = Vec::new();
    let mut templates: Vec<String> = Vec::new();
    for t in obj
        .get("transforms")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if t.get("mode").and_then(Value::as_str) != Some("sql") {
            report.push(note(
                "a transform that is not mode \"sql\" was dropped; Queryfirst reads .sql/.psql files only"
                    .into(),
            ));
            continue;
        }
        let incs: Vec<&str> = match t.get("include") {
            Some(Value::String(s)) => vec![s.as_str()],
            Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        for i in incs {
            let i = i.trim_start_matches("./");
            include.push(match &src_dir {
                Some(d) => format!("{d}/{i}"),
                None => i.to_string(),
            });
        }
        if let Some(e) = t.get("emitTemplate").and_then(Value::as_str) {
            templates.push(e.to_string());
        }
    }
    let include = if include.is_empty() {
        report.push(note(
            "found no sql transform include pattern; using the default include".into(),
        ));
        None
    } else {
        Some(include)
    };

    // emitTemplate: .queries.ts -> .query.ts
    let emit_template = match templates.first() {
        Some(first) => {
            if templates.iter().any(|t| t != first) {
                report.push(note(
                    "the transforms use different emitTemplate values; only the first was kept"
                        .into(),
                ));
            }
            match first.strip_suffix(".queries.ts") {
                Some(stem) => format!("{stem}.query.ts"),
                None => first.clone(),
            }
        }
        None => DEFAULT_EMIT_TEMPLATE.to_string(),
    };

    let types_overrides: BTreeMap<String, Value> = obj
        .get("typesOverrides")
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    let camel_case_column_names = obj.get("camelCaseColumnNames") == Some(&Value::Bool(true));

    // Dropped settings, each with the reason.
    let dropped = [
        (
            "failOnError",
            "qf always exits 1 when a query has errors, and 0 otherwise",
        ),
        (
            "maxWorkerThreads",
            "describes run in one process, in parallel up to `pool` connections (default 8)",
        ),
        (
            "db",
            "connection comes from PG* env vars or DATABASE_URL; never stored in config",
        ),
    ];
    for (key, why) in dropped {
        if obj.contains_key(key) {
            report.push(note(format!("`{key}` dropped: {why}")));
        }
    }

    Ok(OutConfig {
        schema: SCHEMA_REF,
        include,
        exclude: None,
        generators: vec![OutGenerator {
            name: "typescript",
            emit_template,
            types_overrides,
            camel_case_column_names,
        }],
    })
}

/// Find the configuration object literal in `.pgtyped.ts` and read it as far as it is literal.
/// Returns the value (expressions read as null) and the dotted paths of the expressions.
fn scan_ts(src: &str) -> Option<(Value, Vec<String>)> {
    let chars: Vec<char> = src.chars().collect();
    let brace_after = |from: usize| {
        let mut j = from;
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        (chars.get(j) == Some(&'{')).then_some(j)
    };
    // `= {` (const config: IConfig = {, module.exports = {), else `export default {`.
    let mut start = (0..chars.len())
        .filter(|&i| chars[i] == '=')
        .find_map(|i| brace_after(i + 1));
    if start.is_none() {
        let at = src.find("export default")?;
        start = brace_after(src[..at].chars().count() + "export default".len());
    }
    let mut p = Js {
        s: chars,
        i: start?,
        unreadable: Vec::new(),
    };
    let v = p.value("")?;
    Some((v, p.unreadable))
}

struct Js {
    s: Vec<char>,
    i: usize,
    unreadable: Vec<String>,
}

impl Js {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn ws(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.i += 1;
            } else if c == '/' && self.s.get(self.i + 1) == Some(&'/') {
                while self.peek().is_some_and(|c| c != '\n') {
                    self.i += 1;
                }
            } else if c == '/' && self.s.get(self.i + 1) == Some(&'*') {
                self.i += 2;
                while self.i < self.s.len()
                    && !(self.s[self.i] == '*' && self.s.get(self.i + 1) == Some(&'/'))
                {
                    self.i += 1;
                }
                self.i = (self.i + 2).min(self.s.len());
            } else {
                break;
            }
        }
    }

    /// A quoted string; the cursor is on the opening quote. `None` for a template literal with
    /// `${...}` in it (the cursor is then undefined; callers reset it).
    fn string(&mut self) -> Option<String> {
        let q = self.peek()?;
        self.i += 1;
        let mut out = String::new();
        loop {
            let c = self.peek()?;
            self.i += 1;
            match c {
                c if c == q => return Some(out),
                '\\' => {
                    let e = self.peek()?;
                    self.i += 1;
                    out.push(match e {
                        'n' => '\n',
                        't' => '\t',
                        other => other,
                    });
                }
                '$' if q == '`' && self.peek() == Some('{') => return None,
                c => out.push(c),
            }
        }
    }

    /// Skip an expression up to the next `,` `}` or `]` at depth 0.
    fn skip_expr(&mut self) {
        let mut depth = 0usize;
        while let Some(c) = self.peek() {
            match c {
                '"' | '\'' | '`' => {
                    self.i += 1;
                    while let Some(d) = self.peek() {
                        self.i += 1;
                        if d == '\\' {
                            self.i += 1;
                        } else if d == c {
                            break;
                        }
                    }
                    continue;
                }
                '(' | '[' | '{' => depth += 1,
                ')' => depth = depth.saturating_sub(1),
                ']' | '}' => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                }
                ',' if depth == 0 => return,
                _ => {}
            }
            self.i += 1;
        }
    }

    fn mark(&mut self, path: &str) -> Value {
        self.skip_expr();
        self.unreadable.push(path.to_string());
        Value::Null
    }

    fn value(&mut self, path: &str) -> Option<Value> {
        self.ws();
        let c = self.peek()?;
        let at = self.i;
        let v = match c {
            '{' => return self.object(path),
            '[' => return self.array(path),
            '"' | '\'' | '`' => match self.string() {
                Some(s) => Value::String(s),
                None => {
                    self.i = at;
                    return Some(self.mark(path));
                }
            },
            _ => {
                let mut word = String::new();
                while let Some(c) = self.peek() {
                    if c.is_alphanumeric() || matches!(c, '_' | '$' | '.' | '-' | '+') {
                        word.push(c);
                        self.i += 1;
                    } else {
                        break;
                    }
                }
                match word.as_str() {
                    "true" => Value::Bool(true),
                    "false" => Value::Bool(false),
                    "null" => Value::Null,
                    w => match w.parse::<serde_json::Number>() {
                        Ok(n) => Value::Number(n),
                        Err(_) => {
                            self.i = at;
                            return Some(self.mark(path));
                        }
                    },
                }
            }
        };
        self.ws();
        if matches!(self.peek(), None | Some(',' | '}' | ']')) {
            Some(v)
        } else {
            // A literal inside a larger expression (`"a" + b`, `1 * 2`).
            self.i = at;
            Some(self.mark(path))
        }
    }

    fn array(&mut self, path: &str) -> Option<Value> {
        self.i += 1;
        let mut items = Vec::new();
        loop {
            self.ws();
            match self.peek()? {
                ']' => {
                    self.i += 1;
                    return Some(Value::Array(items));
                }
                ',' => self.i += 1,
                _ => items.push(self.value(&format!("{path}[{}]", items.len()))?),
            }
        }
    }

    fn object(&mut self, path: &str) -> Option<Value> {
        self.i += 1;
        let mut map = serde_json::Map::new();
        loop {
            self.ws();
            match self.peek()? {
                '}' => {
                    self.i += 1;
                    return Some(Value::Object(map));
                }
                ',' => {
                    self.i += 1;
                    continue;
                }
                _ => {}
            }
            let key = if matches!(self.peek()?, '"' | '\'') {
                self.string()?
            } else {
                let mut k = String::new();
                while let Some(c) = self.peek() {
                    if c.is_alphanumeric() || matches!(c, '_' | '$') {
                        k.push(c);
                        self.i += 1;
                    } else {
                        break;
                    }
                }
                k
            };
            let child = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            self.ws();
            if key.is_empty() || self.peek() != Some(':') {
                // A spread (`...x`), shorthand (`x,`) or method: not a literal.
                self.unreadable
                    .push(if key.is_empty() { path.into() } else { child });
                let before = self.i;
                self.skip_expr();
                if self.i == before {
                    self.i += 1;
                }
                continue;
            }
            self.i += 1;
            let v = self.value(&child)?;
            map.insert(key, v);
        }
    }
}
