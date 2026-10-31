//! Test helpers shared by the nullability tests (and later analyzer tests): the fixture format
//! and an in-memory catalog that mirrors `tests/fixtures/schema.sql`.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use qf_meta::{NullableReason, TypeCategory, TypeRef};
use qf_pg::{CatalogColumn, CatalogView, ColumnNullability};

/// One `.sql` fixture: its first line is `-- expect: name=notnull other=nullable:outer-join …`.
#[derive(Debug, Clone)]
pub struct Fixture {
    pub name: String,
    pub sql: String,
    /// Result column names as Postgres reports them (alias suffixes `!`/`?` included), in order,
    /// with the expected nullability after alias overrides.
    pub expect: Vec<(String, ColumnNullability)>,
}

pub fn fixtures(dir: &str) -> Vec<Fixture> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(dir);
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "sql"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no fixtures in {}", dir.display());
    paths
        .iter()
        .map(|p| {
            let sql = std::fs::read_to_string(p).unwrap().replace("\r\n", "\n");
            let name = p.file_stem().unwrap().to_string_lossy().into_owned();
            let first = sql.lines().next().unwrap_or_default();
            let spec = first
                .strip_prefix("-- expect:")
                .unwrap_or_else(|| panic!("{name}: first line must be `-- expect: …`"));
            let expect = parse_expect(spec).unwrap_or_else(|e| panic!("{name}: {e}"));
            Fixture { name, sql, expect }
        })
        .collect()
}

/// Parse `a=notnull "b!"=notnull:override c=nullable:outer-join`. Names may be double-quoted;
/// `notnull` alone means proven-notnull; `nullable` needs a reason.
pub fn parse_expect(spec: &str) -> Result<Vec<(String, ColumnNullability)>, String> {
    let mut out = Vec::new();
    for item in spec.split_whitespace() {
        let (name, value) = if let Some(rest) = item.strip_prefix('"') {
            let end = rest
                .find("\"=")
                .ok_or_else(|| format!("bad item `{item}`"))?;
            (&rest[..end], &rest[end + 2..])
        } else {
            item.split_once('=')
                .ok_or_else(|| format!("bad item `{item}`"))?
        };
        let (state, reason) = match value.split_once(':') {
            Some((s, r)) => (s, Some(r)),
            None => (value, None),
        };
        let nullable = match state {
            "notnull" => false,
            "nullable" => true,
            _ => return Err(format!("bad state in `{item}`")),
        };
        let reason = match (nullable, reason) {
            (false, None) => NullableReason::ProvenNotnull,
            (true, None) => return Err(format!("`{item}`: nullable needs a reason")),
            (_, Some(r)) => reason_from(r).ok_or_else(|| format!("bad reason in `{item}`"))?,
        };
        out.push((name.to_string(), ColumnNullability { nullable, reason }));
    }
    if out.is_empty() {
        return Err("empty expectation".into());
    }
    Ok(out)
}

fn reason_from(s: &str) -> Option<NullableReason> {
    Some(match s {
        "proven-notnull" => NullableReason::ProvenNotnull,
        "outer-join" => NullableReason::OuterJoin,
        "expression" => NullableReason::Expression,
        "analyzer-gave-up" => NullableReason::AnalyzerGaveUp,
        "override" => NullableReason::Override,
        _ => return None,
    })
}

pub fn show(c: &ColumnNullability) -> String {
    let reason = match c.reason {
        NullableReason::ProvenNotnull => "proven-notnull",
        NullableReason::OuterJoin => "outer-join",
        NullableReason::Expression => "expression",
        NullableReason::AnalyzerGaveUp => "analyzer-gave-up",
        NullableReason::Override => "override",
    };
    format!(
        "{}:{reason}",
        if c.nullable { "nullable" } else { "notnull" }
    )
}

/// The output name once the alias override suffix is stripped.
pub fn stripped(name: &str) -> &str {
    name.strip_suffix(['!', '?'])
        .filter(|s| !s.is_empty())
        .unwrap_or(name)
}

/// An in-memory [`CatalogView`] with the tables of `tests/fixtures/schema.sql`, in schema
/// `public` (the only schema on its search path). Table oids are 1, 2, 3; attnums count from 1.
/// (schema, table, [(column, not null)]).
type FakeTable = (&'static str, &'static str, Vec<(&'static str, bool)>);

pub struct FakeCatalog {
    tables: Vec<FakeTable>,
}

impl FakeCatalog {
    pub fn fixture_schema() -> FakeCatalog {
        FakeCatalog {
            tables: vec![
                (
                    "public",
                    "customers",
                    vec![
                        ("id", true),
                        ("email", true),
                        ("name", false),
                        ("access", true),
                        ("tags", false),
                        ("prefs", false),
                        ("created_at", true),
                        ("birthday", false),
                    ],
                ),
                (
                    "public",
                    "addresses",
                    vec![
                        ("id", true),
                        ("customer_id", true),
                        ("line1", true),
                        ("city", false),
                        ("postcode", false),
                    ],
                ),
                (
                    "public",
                    "invoices",
                    vec![
                        ("id", true),
                        ("customer_id", true),
                        ("address_id", false),
                        ("total", true),
                        ("issued_on", true),
                        ("notes", false),
                    ],
                ),
            ],
        }
    }

    /// (oid, attnum) of a column, for building `Described` sources by hand.
    pub fn source(&self, table: &str, column: &str) -> Option<(u32, i16)> {
        let t = self.tables.iter().position(|t| t.1 == table)?;
        let c = self.tables[t].2.iter().position(|c| c.0 == column)?;
        Some((t as u32 + 1, c as i16 + 1))
    }

    fn col(&self, t: usize, c: usize) -> CatalogColumn {
        let (schema, table, cols) = &self.tables[t];
        CatalogColumn {
            schema: schema.to_string(),
            table: table.to_string(),
            name: cols[c].0.to_string(),
            not_null: cols[c].1,
            has_default: false,
            db_type: dummy_type(),
        }
    }

    fn find(&self, schema: Option<&str>, table: &str) -> Option<usize> {
        self.tables
            .iter()
            .position(|t| t.1 == table && schema.is_none_or(|s| s == t.0))
    }
}

impl CatalogView for FakeCatalog {
    fn column_by_name(
        &self,
        schema: Option<&str>,
        table: &str,
        column: &str,
    ) -> Option<CatalogColumn> {
        let t = self.find(schema, table)?;
        let c = self.tables[t].2.iter().position(|c| c.0 == column)?;
        Some(self.col(t, c))
    }

    fn column(&self, table_oid: u32, attnum: i16) -> Option<CatalogColumn> {
        let t = (table_oid as usize).checked_sub(1)?;
        let c = usize::try_from(attnum).ok()?.checked_sub(1)?;
        (t < self.tables.len() && c < self.tables[t].2.len()).then(|| self.col(t, c))
    }

    fn table_columns(&self, schema: Option<&str>, table: &str) -> Option<Vec<CatalogColumn>> {
        let t = self.find(schema, table)?;
        Some(
            (0..self.tables[t].2.len())
                .map(|c| self.col(t, c))
                .collect(),
        )
    }
}

pub fn dummy_type() -> TypeRef {
    TypeRef {
        schema: "pg_catalog".into(),
        name: "text".into(),
        array: false,
        modifier: None,
        category: TypeCategory::Base,
        enum_values: None,
    }
}

#[cfg(feature = "db-tests")]
pub mod db;
