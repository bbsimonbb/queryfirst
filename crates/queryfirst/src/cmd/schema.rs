//! `qf schema [table…] [--db <conn>]`: show the real database schema compactly, so SQL can be
//! written against it. Read-only: the catalog is read with a few `SELECT`s.
//!
//! With no table: one `schema.name (kind)` line per table or view. With tables: columns (type,
//! `NOT NULL` or `null`, default), primary key, foreign keys and the values of the enums used.
//! Types use the meta files' notation (`numeric(12,2)`, `text[]`).
//!
//! `NOT NULL` is what the database declares. Where the analyzer does not rely on it (NOT VALID,
//! NO INHERIT, a foreign table, a descendant whose own rows may hold NULL) it reads
//! `NOT NULL (not enforced everywhere)`.
//!
//! stdout carries only the schema (text, or with `--json` one document); diagnostics go to
//! stderr.

use std::path::Path;

use qf_meta::{TypeCategory, TypeRef, fmt_type};
use qf_pg::{
    Catalog, ConnError, SchemaColumn, SchemaInfo, SchemaTable, connect_pool, resolve_connection,
};
use serde_json::{Value, json};

use crate::cmd::generate::{CONNECT_TIMEOUT, connection_diagnostic, connection_sources};
use crate::config::{CONFIG_FILE, load_config};
use crate::report::{Report, ReportedDiagnostic, print_report};

const NOT_ENFORCED: &str = "NOT NULL (not enforced everywhere)";

/// `schema.name (kind)` per table, one per line.
pub fn render_list(info: &SchemaInfo) -> String {
    info.tables
        .iter()
        .map(|t| format!("{} ({})\n", t.qualified(), t.kind_name()))
        .collect()
}

fn nullability(c: &SchemaColumn) -> &'static str {
    match (c.not_null, c.not_null_trusted) {
        (false, _) => "null",
        (true, true) => "NOT NULL",
        (true, false) => NOT_ENFORCED,
    }
}

/// `default <expr>`; identity and generated columns are shown as the catalog phrases them.
fn default_text(d: &str) -> String {
    if d.starts_with("GENERATED ") {
        d.to_string()
    } else {
        format!("default {d}")
    }
}

/// The enum a type is, or is an array of, as `(name, values)`.
fn enum_of(t: &TypeRef) -> Option<(String, &[String])> {
    if t.category != TypeCategory::Enum {
        return None;
    }
    let bare = TypeRef {
        array: false,
        modifier: None,
        enum_values: None,
        ..t.clone()
    };
    Some((fmt_type(&bare), t.enum_values.as_deref()?))
}

/// The enums used by `tables`' columns, sorted by name and deduplicated.
fn enums_used<'a>(tables: &[&'a SchemaTable]) -> Vec<(String, &'a [String])> {
    let mut v: Vec<_> = tables
        .iter()
        .flat_map(|t| t.columns.iter().filter_map(|c| enum_of(&c.db_type)))
        .collect();
    v.sort();
    v.dedup();
    v
}

fn render_table(out: &mut String, t: &SchemaTable) {
    out.push_str(&format!("{} ({})\n", t.qualified(), t.kind_name()));
    let types: Vec<String> = t.columns.iter().map(|c| fmt_type(&c.db_type)).collect();
    let width = |it: &mut dyn Iterator<Item = usize>| it.max().unwrap_or(0);
    let w0 = width(&mut t.columns.iter().map(|c| c.name.chars().count()));
    let w1 = width(&mut types.iter().map(|s| s.chars().count()));
    let w2 = width(&mut t.columns.iter().map(|c| nullability(c).len()));
    for (c, ty) in t.columns.iter().zip(&types) {
        let line = match &c.default {
            Some(d) => format!(
                "  {:<w0$}  {:<w1$}  {:<w2$}  {}",
                c.name,
                ty,
                nullability(c),
                default_text(d)
            ),
            None => format!("  {:<w0$}  {:<w1$}  {}", c.name, ty, nullability(c)),
        };
        out.push_str(&line);
        out.push('\n');
    }
    if !t.primary_key.is_empty() {
        out.push_str(&format!("  PK ({})\n", t.primary_key.join(", ")));
    }
    for fk in &t.foreign_keys {
        out.push_str(&format!(
            "  FK ({}) → {}.{}({})\n",
            fk.columns.join(", "),
            fk.ref_schema,
            fk.ref_table,
            fk.ref_columns.join(", ")
        ));
    }
    for (name, values) in enums_used(&[t]) {
        out.push_str(&format!("  enum {name}: {}\n", values.join(" | ")));
    }
}

/// The detail of each table, separated by a blank line.
pub fn render_tables(tables: &[&SchemaTable]) -> String {
    let mut out = String::new();
    for (i, t) in tables.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        render_table(&mut out, t);
    }
    out
}

/// `{"tables":[{schema,name,kind}],"enums":[]}`.
pub fn json_list(info: &SchemaInfo) -> Value {
    json!({
        "tables": info.tables.iter().map(|t| json!({
            "schema": t.schema,
            "name": t.name,
            "kind": t.kind_name(),
        })).collect::<Vec<_>>(),
        "enums": [],
    })
}

/// The full document for the given tables.
pub fn json_tables(tables: &[&SchemaTable]) -> Value {
    json!({
        "tables": tables.iter().map(|t| json!({
            "schema": t.schema,
            "name": t.name,
            "kind": t.kind_name(),
            "columns": t.columns.iter().map(|c| json!({
                "name": c.name,
                "type": fmt_type(&c.db_type),
                "nullable": !c.not_null,
                "default": c.default,
            })).collect::<Vec<_>>(),
            "primaryKey": t.primary_key,
            "foreignKeys": t.foreign_keys.iter().map(|fk| json!({
                "columns": fk.columns,
                "references": {
                    "table": format!("{}.{}", fk.ref_schema, fk.ref_table),
                    "columns": fk.ref_columns,
                },
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "enums": enums_used(tables).into_iter()
            .map(|(name, values)| json!({"name": name, "values": values}))
            .collect::<Vec<_>>(),
    })
}

/// The named tables in the order given. An unknown one is a QF091 usage error, with the closest
/// names when there are any.
pub fn select<'a>(
    info: &'a SchemaInfo,
    names: &[String],
) -> Result<Vec<&'a SchemaTable>, ReportedDiagnostic> {
    names
        .iter()
        .map(|n| {
            info.find(n).ok_or_else(|| {
                let near: Vec<String> =
                    info.suggest(n).iter().map(|m| format!("\"{m}\"")).collect();
                let mut message = format!("unknown table \"{n}\"");
                if !near.is_empty() {
                    message.push_str(&format!("; did you mean {}?", near.join(" or ")));
                }
                ReportedDiagnostic::project(
                    "QF091",
                    message,
                    Some("list the tables with `qf schema`; qualify as schema.table".into()),
                )
            })
        })
        .collect()
}

fn pretty(v: &Value) -> String {
    format!(
        "{}\n",
        serde_json::to_string_pretty(v).expect("schema serialises")
    )
}

/// What to print on stdout for `info` and the requested table names, or the usage error.
pub fn render(
    info: &SchemaInfo,
    names: &[String],
    json: bool,
) -> Result<String, ReportedDiagnostic> {
    if names.is_empty() {
        return Ok(if json {
            pretty(&json_list(info))
        } else {
            render_list(info)
        });
    }
    let tables = select(info, names)?;
    Ok(if json {
        pretty(&json_tables(&tables))
    } else {
        render_tables(&tables)
    })
}

fn fatal(d: ReportedDiagnostic, json: bool) -> u8 {
    let mut report = Report::new("schema");
    report.push(d);
    report.finish();
    print_report(&report, json);
    report.exit_code()
}

fn load(project: &Path, db: Option<&str>) -> Result<SchemaInfo, ReportedDiagnostic> {
    let cfg = load_config(project).map_err(|d| ReportedDiagnostic::new(Some(CONFIG_FILE), &d))?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ReportedDiagnostic::project("QF091", format!("cannot start: {e}"), None))?;
    rt.block_on(async {
        let stop = |e: ConnError| connection_diagnostic(&e);
        let conn = resolve_connection(&connection_sources(&cfg, db)).map_err(stop)?;
        let pool = connect_pool(&conn, 1, CONNECT_TIMEOUT)
            .await
            .map_err(stop)?;
        let cat = Catalog::load(&pool).await.map_err(stop)?;
        Ok(cat.schema_info())
    })
}

/// Run `qf schema`, print the result and return the exit code.
pub fn run(project: &Path, tables: &[String], db: Option<&str>, json: bool) -> u8 {
    let info = match load(project, db) {
        Ok(i) => i,
        Err(d) => return fatal(d, json),
    };
    match render(&info, tables, json) {
        Ok(text) => {
            print!("{text}");
            0
        }
        Err(d) => fatal(d, json),
    }
}
