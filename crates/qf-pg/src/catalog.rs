//! The whole catalog, loaded once per run with a few `SELECT`s so that no query needs another
//! round-trip (the database is often behind a slow tunnel).

use std::collections::HashMap;

use qf_meta::{TypeCategory, TypeRef};

use crate::connect::{ConnError, Pool, describe_pg_error};
use crate::format_modifier;

/// A column of a table, view or materialised view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogColumn {
    pub schema: String,
    pub table: String,
    pub name: String,
    pub not_null: bool,
    /// Has a default, or is an identity or generated column.
    pub has_default: bool,
    pub db_type: TypeRef,
}

/// A table, view, materialised view or foreign table, with its columns in `attnum` order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogTable {
    pub oid: u32,
    pub schema: String,
    pub name: String,
    /// `relkind`: `r`, `p`, `v`, `m` or `f`.
    pub kind: char,
    pub columns: Vec<CatalogColumn>,
}

#[derive(Debug, Clone)]
struct TypeInfo {
    schema: String,
    name: String,
    typtype: char,
    category: char,
    elem: u32,
}

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    types: HashMap<u32, TypeInfo>,
    /// Enum labels by type oid, sorted.
    enums: HashMap<u32, Vec<String>>,
    tables: Vec<CatalogTable>,
    /// (table oid, attnum) -> (index in `tables`, index in its columns).
    columns: HashMap<(u32, i16), (usize, usize)>,
    by_name: HashMap<(String, String), usize>,
    search_path: Vec<String>,
    timezone: String,
}

const TYPES_SQL: &str = "\
SELECT t.oid, n.nspname::text, t.typname::text, t.typtype::text, t.typcategory::text, t.typelem
FROM pg_catalog.pg_type t
JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace";

const ENUMS_SQL: &str = "SELECT enumtypid, enumlabel::text FROM pg_catalog.pg_enum";

const COLUMNS_SQL: &str = "\
SELECT c.oid, n.nspname::text, c.relname::text, c.relkind::text,
       a.attnum, a.attname::text, a.atttypid, a.atttypmod, a.attnotnull,
       (EXISTS (SELECT 1 FROM pg_catalog.pg_attrdef d
                WHERE d.adrelid = a.attrelid AND d.adnum = a.attnum)
        OR a.attidentity <> '' OR a.attgenerated <> '') AS has_default
FROM pg_catalog.pg_attribute a
JOIN pg_catalog.pg_class c ON c.oid = a.attrelid
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
WHERE a.attnum > 0 AND NOT a.attisdropped
  AND c.relkind IN ('r', 'p', 'v', 'm', 'f')
  AND n.nspname NOT IN ('pg_catalog', 'information_schema')
  AND n.nspname NOT LIKE 'pg\\_toast%'
ORDER BY c.oid, a.attnum";

const SETTINGS_SQL: &str = "SELECT current_setting('TimeZone'), current_schemas(false)::text[]";

fn first_char(s: &str) -> char {
    s.chars().next().unwrap_or(' ')
}

impl Catalog {
    /// Load types, enums, columns, the search path and the session time zone.
    pub async fn load(pool: &Pool) -> Result<Catalog, ConnError> {
        let client = pool.get().await;
        let fail = |e: tokio_postgres::Error| {
            if e.as_db_error().is_some() {
                ConnError::new(
                    "QF090",
                    format!("cannot read the catalog at {}", pool.target()),
                )
                .with_detail(describe_pg_error(&e))
            } else {
                ConnError::unreachable(pool.target(), describe_pg_error(&e))
            }
        };

        let mut cat = Catalog::default();
        for row in client.query(TYPES_SQL, &[]).await.map_err(fail)? {
            cat.types.insert(
                row.get(0),
                TypeInfo {
                    schema: row.get(1),
                    name: row.get(2),
                    typtype: first_char(row.get::<_, &str>(3)),
                    category: first_char(row.get::<_, &str>(4)),
                    elem: row.get(5),
                },
            );
        }
        for row in client.query(ENUMS_SQL, &[]).await.map_err(fail)? {
            cat.enums.entry(row.get(0)).or_default().push(row.get(1));
        }
        for labels in cat.enums.values_mut() {
            labels.sort();
        }

        let settings = client.query_one(SETTINGS_SQL, &[]).await.map_err(fail)?;
        cat.timezone = settings.get(0);
        cat.search_path = settings.get(1);

        for row in client.query(COLUMNS_SQL, &[]).await.map_err(fail)? {
            let oid: u32 = row.get(0);
            let schema: String = row.get(1);
            let table: String = row.get(2);
            let t = match cat.tables.last() {
                Some(last) if last.oid == oid => cat.tables.len() - 1,
                _ => {
                    cat.by_name
                        .insert((schema.clone(), table.clone()), cat.tables.len());
                    cat.tables.push(CatalogTable {
                        oid,
                        schema: schema.clone(),
                        name: table.clone(),
                        kind: first_char(row.get::<_, &str>(3)),
                        columns: Vec::new(),
                    });
                    cat.tables.len() - 1
                }
            };
            let attnum: i16 = row.get(4);
            let column = CatalogColumn {
                schema,
                table,
                name: row.get(5),
                db_type: cat.type_ref(row.get(6), row.get(7)),
                not_null: row.get(8),
                has_default: row.get(9),
            };
            let cols = &mut cat.tables[t].columns;
            cat.columns.insert((oid, attnum), (t, cols.len()));
            cols.push(column);
        }
        Ok(cat)
    }

    /// The [`TypeRef`] for a type oid and modifier. Arrays get the element's name with
    /// `array = true`. An oid not in the catalog gives `pg_catalog.unknown`.
    pub fn type_ref(&self, oid: u32, typmod: i32) -> TypeRef {
        match self.types.get(&oid) {
            Some(t) if t.category == 'A' && t.elem != 0 => TypeRef {
                array: true,
                ..self.type_ref(t.elem, typmod)
            },
            Some(t) => TypeRef {
                schema: t.schema.clone(),
                name: t.name.clone(),
                array: false,
                modifier: format_modifier(&t.name, typmod),
                category: match t.typtype {
                    'b' => TypeCategory::Base,
                    'e' => TypeCategory::Enum,
                    'd' => TypeCategory::Domain,
                    'c' => TypeCategory::Composite,
                    _ => TypeCategory::Other,
                },
                enum_values: (t.typtype == 'e')
                    .then(|| self.enums.get(&oid).cloned().unwrap_or_default()),
            },
            None => TypeRef {
                schema: "pg_catalog".into(),
                name: "unknown".into(),
                array: false,
                modifier: None,
                category: TypeCategory::Other,
                enum_values: None,
            },
        }
    }

    pub fn column(&self, table_oid: u32, attnum: i16) -> Option<CatalogColumn> {
        let &(t, c) = self.columns.get(&(table_oid, attnum))?;
        Some(self.tables[t].columns[c].clone())
    }

    /// A table by name; without a schema, the first match on the search path.
    pub fn table(&self, schema: Option<&str>, name: &str) -> Option<CatalogTable> {
        self.table_ref(schema, name).cloned()
    }

    pub(crate) fn table_ref(&self, schema: Option<&str>, name: &str) -> Option<&CatalogTable> {
        let find = |s: &str| self.by_name.get(&(s.to_string(), name.to_string()));
        let i = match schema {
            Some(s) => find(s)?,
            None => self.search_path.iter().find_map(|s| find(s))?,
        };
        Some(&self.tables[*i])
    }

    /// Table names in `schema`, or on the search path, for did-you-mean.
    pub(crate) fn table_names(&self, schema: Option<&str>) -> Vec<String> {
        self.tables
            .iter()
            .filter(|t| match schema {
                Some(s) => t.schema == s,
                None => self.search_path.contains(&t.schema),
            })
            .map(|t| t.name.clone())
            .collect()
    }

    /// The session `TimeZone` setting.
    pub fn timezone(&self) -> &str {
        &self.timezone
    }
}
