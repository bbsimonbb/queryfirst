//! `queryfirst.json`: the project configuration, documented by `schema/queryfirst.schema.json`.
//!
//! A missing file means all defaults. Unknown keys and invalid values are `QF091` (exit 2).

use std::collections::BTreeMap;
use std::path::Path;

use qf_gen_ts::{TsOptions, TypeOverrideOpt};
use qf_sqlfile::{Diagnostic, Severity};
use serde::Deserialize;

pub const CONFIG_FILE: &str = "queryfirst.json";
pub const DEFAULT_EMIT_TEMPLATE: &str = "{{dir}}/{{name}}.qf.ts";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Only `"postgres"` for now.
    pub provider: String,
    /// Globs, matched against project-relative POSIX paths.
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub connection: Option<ConnConfig>,
    /// Connection pool size; describes run in parallel up to this many.
    pub pool: usize,
    /// `"utc"`: how `timestamp` (without time zone) is read (phase 2).
    pub timestamp: Option<String>,
    pub test: Option<TestConfig>,
    pub generators: Vec<GeneratorConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnConfig {
    /// Environment variable holding the connection string.
    pub env: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestConfig {
    pub connection: Option<TestConnConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestConnConfig {
    pub env: Option<String>,
    pub command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratorConfig {
    /// Only `"typescript"` for now.
    pub name: String,
    pub emit_template: String,
    pub types_overrides: BTreeMap<String, TypeOverrideOpt>,
    pub camel_case_column_names: bool,
}

impl GeneratorConfig {
    pub fn typescript() -> Self {
        GeneratorConfig {
            name: "typescript".into(),
            emit_template: DEFAULT_EMIT_TEMPLATE.into(),
            types_overrides: BTreeMap::new(),
            camel_case_column_names: false,
        }
    }

    pub fn ts_options(&self) -> TsOptions {
        TsOptions {
            types_overrides: self.types_overrides.clone(),
            camel_case_column_names: self.camel_case_column_names,
            emit_template: self.emit_template.clone(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            provider: "postgres".into(),
            include: vec!["**/*.psql".into(), "**/*.sql".into()],
            exclude: vec!["**/node_modules/**".into()],
            connection: None,
            pool: 8,
            timestamp: None,
            test: None,
            generators: vec![GeneratorConfig::typescript()],
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawConfig {
    #[serde(rename = "$schema")]
    _schema: Option<String>,
    provider: Option<String>,
    include: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
    connection: Option<ConnConfig>,
    pool: Option<usize>,
    timestamp: Option<String>,
    test: Option<TestConfig>,
    generators: Option<Vec<RawGenerator>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawGenerator {
    name: String,
    emit_template: Option<String>,
    types_overrides: Option<BTreeMap<String, RawTypeOverride>>,
    camel_case_column_names: Option<bool>,
}

/// pgtyped's two `typesOverrides` shapes.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawTypeOverride {
    Both(String),
    Split(RawSplit),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSplit {
    parameter: Option<String>,
    #[serde(rename = "return")]
    ret: Option<String>,
}

fn config_error(message: String, line: u32, col: u32) -> Diagnostic {
    Diagnostic {
        code: "QF091",
        severity: Severity::Error,
        message,
        hint: Some(format!(
            "see the {CONFIG_FILE} schema (schema/queryfirst.schema.json)"
        )),
        line: line.max(1),
        col: col.max(1),
    }
}

/// Load `queryfirst.json` from `project`; all defaults when there is none.
pub fn load_config(project: &Path) -> Result<Config, Diagnostic> {
    let path = project.join(CONFIG_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => parse_config(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(config_error(
            format!("cannot read {CONFIG_FILE}: {e}"),
            1,
            1,
        )),
    }
}

/// Parse and validate the text of a `queryfirst.json`.
pub fn parse_config(text: &str) -> Result<Config, Diagnostic> {
    let raw: RawConfig = serde_json::from_str(text).map_err(|e| {
        let msg = e.to_string();
        // serde_json appends " at line L column C"; the position goes in the diagnostic.
        let msg = msg
            .rsplit_once(" at line ")
            .map_or(msg.as_str(), |(m, _)| m);
        let msg = if msg.starts_with("data did not match any variant of untagged enum") {
            "invalid typesOverrides entry: expected a type name or {\"parameter\", \"return\"}"
                .to_string()
        } else {
            format!("invalid {CONFIG_FILE}: {msg}")
        };
        config_error(msg, e.line() as u32, e.column() as u32)
    })?;
    let d = Config::default();
    let invalid = |m: String| config_error(m, 1, 1);

    let provider = raw.provider.unwrap_or(d.provider);
    if provider != "postgres" {
        return Err(invalid(format!(
            "unsupported provider \"{provider}\" (supported: \"postgres\")"
        )));
    }
    let pool = raw.pool.unwrap_or(d.pool);
    if pool == 0 {
        return Err(invalid("pool must be at least 1".into()));
    }
    if let Some(t) = &raw.timestamp
        && t != "utc"
    {
        return Err(invalid(format!(
            "unsupported timestamp setting \"{t}\" (supported: \"utc\")"
        )));
    }
    let include = raw.include.unwrap_or(d.include);
    let exclude = raw.exclude.unwrap_or(d.exclude);
    for g in include.iter().chain(&exclude) {
        if let Err(e) = crate::discover::glob(g) {
            return Err(invalid(format!("invalid glob \"{g}\": {e}")));
        }
    }
    let generators = match raw.generators {
        None => d.generators,
        Some(gs) => gs
            .into_iter()
            .map(|g| {
                if g.name != "typescript" {
                    return Err(invalid(format!(
                        "unknown generator \"{}\" (supported: \"typescript\")",
                        g.name
                    )));
                }
                let base = GeneratorConfig::typescript();
                Ok(GeneratorConfig {
                    name: g.name,
                    emit_template: g.emit_template.unwrap_or(base.emit_template),
                    types_overrides: g
                        .types_overrides
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(k, v)| {
                            let v = match v {
                                RawTypeOverride::Both(s) => TypeOverrideOpt::Both(s),
                                RawTypeOverride::Split(s) => TypeOverrideOpt::Split {
                                    parameter: s.parameter,
                                    ret: s.ret,
                                },
                            };
                            (k, v)
                        })
                        .collect(),
                    camel_case_column_names: g.camel_case_column_names.unwrap_or(false),
                })
            })
            .collect::<Result<_, _>>()?,
    };
    Ok(Config {
        provider,
        include,
        exclude,
        connection: raw.connection,
        pool,
        timestamp: raw.timestamp,
        test: raw.test,
        generators,
    })
}
