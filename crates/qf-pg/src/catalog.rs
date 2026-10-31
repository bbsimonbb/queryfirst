//! The whole catalog, loaded once per run with a few `SELECT`s so that no query needs another
//! round-trip (the database is often behind a slow tunnel).

use std::collections::{HashMap, HashSet};

use qf_meta::{TypeCategory, TypeRef};

use crate::connect::{ConnError, Pool, describe_pg_error};
use crate::format_modifier;

/// A column of a table, view or materialised view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogColumn {
    pub schema: String,
    pub table: String,
    pub name: String,
    /// `NOT NULL` that holds for every row a scan returns (see `COLUMNS_SQL`).
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
    /// A `BEFORE` or `INSTEAD OF` row trigger or a rewrite rule on `INSERT`, on the relation or
    /// on any descendant: the values an `INSERT` writes may not be the ones it was given.
    pub hooks_insert: bool,
    /// The same for `UPDATE`.
    pub hooks_update: bool,
}

/// A write whose values row hooks (triggers, rules) may replace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOp {
    Insert,
    Update,
}

impl CatalogTable {
    /// Whether a `BEFORE` / `INSTEAD OF` row trigger or a rule may replace the values `op`
    /// writes (a NULL may then be legitimate in a `NOT NULL` column).
    pub fn may_replace_values(&self, op: WriteOp) -> bool {
        match op {
            WriteOp::Insert => self.hooks_insert,
            WriteOp::Update => self.hooks_update,
        }
    }
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
    pub(crate) tables: Vec<CatalogTable>,
    /// (table oid, attnum) -> (index in `tables`, index in its columns).
    pub(crate) columns: HashMap<(u32, i16), (usize, usize)>,
    by_name: HashMap<(String, String), usize>,
    pub(crate) search_path: Vec<String>,
    timezone: String,
    /// What the database declares, for `qf schema`: the plain `attnotnull` (not the
    /// conservative `not_null`), default expressions, and key constraints.
    pub(crate) declared_not_null: HashSet<(u32, i16)>,
    pub(crate) defaults: HashMap<(u32, i16), String>,
    /// Primary key columns (`attnum`s in key order) by table oid.
    pub(crate) primary_keys: HashMap<u32, Vec<i16>>,
    pub(crate) foreign_keys: HashMap<u32, Vec<ForeignKeyRow>>,
}

/// A foreign key as the catalog stores it.
#[derive(Debug, Clone)]
pub(crate) struct ForeignKeyRow {
    pub columns: Vec<i16>,
    pub ref_table: u32,
    pub ref_columns: Vec<i16>,
}

const TYPES_SQL: &str = "\
SELECT t.oid, n.nspname::text, t.typname::text, t.typtype::text, t.typcategory::text, t.typelem
FROM pg_catalog.pg_type t
JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace";

const ENUMS_SQL: &str = "SELECT enumtypid, enumlabel::text FROM pg_catalog.pg_enum";

/// `not_null` is a NOT NULL that holds for every row a scan of the relation can return:
/// - not NOT VALID (PG 18 keeps `attnotnull` for one) and not NO INHERIT (PG 18);
/// - not on a foreign table (its NOT NULL is not enforced);
/// - and the same holds in every descendant (inheritance children and partitions, recursively),
///   since `SELECT … FROM parent` scans them too.
const COLUMNS_SQL: &str = "\
WITH RECURSIVE tree (ancestor, descendant) AS (
    SELECT i.inhparent, i.inhrelid FROM pg_catalog.pg_inherits i
    UNION
    SELECT t.ancestor, i.inhrelid
    FROM tree t JOIN pg_catalog.pg_inherits i ON i.inhparent = t.descendant
),
-- Columns of descendant relations whose own rows may hold NULL.
weak (relid, attname) AS (
    SELECT a.attrelid, a.attname
    FROM pg_catalog.pg_attribute a
    JOIN pg_catalog.pg_class c ON c.oid = a.attrelid
    WHERE a.attnum > 0 AND NOT a.attisdropped
      AND a.attrelid IN (SELECT descendant FROM tree)
      AND (NOT a.attnotnull OR c.relkind = 'f'
           OR EXISTS (SELECT 1 FROM pg_catalog.pg_constraint k
                      WHERE k.conrelid = a.attrelid AND k.contype = 'n'
                        AND (NOT k.convalidated OR k.connoinherit)
                        AND a.attnum = ANY (k.conkey)))
),
-- Ancestor columns (matched by name, as inheritance does) with a weak descendant column.
weak_below (relid, attname) AS (
    SELECT DISTINCT t.ancestor, w.attname
    FROM tree t JOIN weak w ON w.relid = t.descendant
)
SELECT c.oid, n.nspname::text, c.relname::text, c.relkind::text,
       a.attnum, a.attname::text, a.atttypid, a.atttypmod,
       (a.attnotnull AND c.relkind <> 'f'
        AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint k
                        WHERE k.conrelid = a.attrelid AND k.contype = 'n'
                          AND (NOT k.convalidated OR k.connoinherit)
                          AND a.attnum = ANY (k.conkey))
        AND NOT EXISTS (SELECT 1 FROM weak_below b
                        WHERE b.relid = a.attrelid AND b.attname = a.attname)) AS not_null,
       (EXISTS (SELECT 1 FROM pg_catalog.pg_attrdef d
                WHERE d.adrelid = a.attrelid AND d.adnum = a.attnum)
        OR a.attidentity <> '' OR a.attgenerated <> '') AS has_default,
       a.attnotnull,
       CASE WHEN a.attidentity = 'a' THEN 'GENERATED ALWAYS AS IDENTITY'
            WHEN a.attidentity = 'd' THEN 'GENERATED BY DEFAULT AS IDENTITY'
            WHEN a.attgenerated <> '' THEN 'GENERATED ALWAYS AS ('
                 || pg_catalog.pg_get_expr(ad.adbin, ad.adrelid) || ') '
                 || CASE a.attgenerated WHEN 'v' THEN 'VIRTUAL' ELSE 'STORED' END
            ELSE pg_catalog.pg_get_expr(ad.adbin, ad.adrelid) END AS default_expr
FROM pg_catalog.pg_attribute a
JOIN pg_catalog.pg_class c ON c.oid = a.attrelid
LEFT JOIN pg_catalog.pg_attrdef ad ON ad.adrelid = a.attrelid AND ad.adnum = a.attnum
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
WHERE a.attnum > 0 AND NOT a.attisdropped
  AND c.relkind IN ('r', 'p', 'v', 'm', 'f')
  AND n.nspname NOT IN ('pg_catalog', 'information_schema')
  AND n.nspname NOT LIKE 'pg\\_toast%'
ORDER BY c.oid, a.attnum";

/// Primary and foreign keys of relations (`conkey` / `confkey` are `attnum`s).
const KEYS_SQL: &str = "SELECT conrelid, contype::text, conkey, confrelid, confkey
FROM pg_catalog.pg_constraint
WHERE contype IN ('p', 'f') AND conrelid <> 0 AND conkey IS NOT NULL
ORDER BY conrelid, conname";

/// Relations whose written values a row hook may replace before `NOT NULL` is checked, with the
/// operations concerned:
/// - enabled (any mode but `D`) row-level `BEFORE` or `INSTEAD OF` triggers on `INSERT` /
///   `UPDATE` (`tgtype` bits: 1 row, 2 before, 4 insert, 16 update, 64 instead). Internal
///   triggers are skipped unless cloned from a parent's (`tgparentid`, PG 13+);
/// - enabled rewrite rules on `INSERT` / `UPDATE` (`ev_type` '3' / '2'; never `_RETURN`);
/// - and a relation is flagged for its descendants' hooks too: a partitioned parent routes rows
///   into partitions, and an `UPDATE` of an inheritance parent updates its children's rows.
const ROW_HOOKS_SQL: &str = "WITH RECURSIVE tree (ancestor, descendant) AS (
    SELECT i.inhparent, i.inhrelid FROM pg_catalog.pg_inherits i
    UNION
    SELECT t.ancestor, i.inhrelid
    FROM tree t JOIN pg_catalog.pg_inherits i ON i.inhparent = t.descendant
),
own (relid, on_insert, on_update) AS (
    SELECT g.tgrelid, (g.tgtype::int & 4) <> 0, (g.tgtype::int & 16) <> 0
    FROM pg_catalog.pg_trigger g
    WHERE (g.tgtype::int & 1) <> 0
      AND (g.tgtype::int & 66) <> 0
      AND (g.tgtype::int & 20) <> 0
      AND g.tgenabled <> 'D'
      AND (NOT g.tgisinternal OR g.tgparentid <> 0)
    UNION ALL
    SELECT r.ev_class, r.ev_type = '3', r.ev_type = '2'
    FROM pg_catalog.pg_rewrite r
    WHERE r.ev_type IN ('2', '3') AND r.rulename <> '_RETURN' AND r.ev_enabled <> 'D'
)
SELECT a.relid, bool_or(o.on_insert), bool_or(o.on_update)
FROM own o
CROSS JOIN LATERAL (
    SELECT o.relid
    UNION
    SELECT t.ancestor FROM tree t WHERE t.descendant = o.relid
) AS a (relid)
GROUP BY a.relid";

const SETTINGS_SQL: &str = "SELECT current_setting('TimeZone'), current_schemas(false)::text[]";

/// Run one catalog query under the pool's query timeout; a timeout or a lost connection is
/// QF090 "cannot reach database".
async fn timed<T>(
    pool: &Pool,
    query: impl Future<Output = Result<T, tokio_postgres::Error>>,
) -> Result<T, ConnError> {
    match tokio::time::timeout(pool.query_timeout(), query).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) if e.as_db_error().is_some() => Err(ConnError::new(
            "QF090",
            format!("cannot read the catalog at {}", pool.target()),
        )
        .with_detail(describe_pg_error(&e))),
        Ok(Err(e)) => Err(ConnError::unreachable(pool.target(), describe_pg_error(&e))),
        Err(_) => Err(ConnError::unreachable(
            pool.target(),
            format!("no answer within {}s", pool.query_timeout().as_secs_f32()),
        )),
    }
}

fn first_char(s: &str) -> char {
    s.chars().next().unwrap_or(' ')
}

impl Catalog {
    /// Load types, enums, columns, the search path and the session time zone.
    pub async fn load(pool: &Pool) -> Result<Catalog, ConnError> {
        let client = pool.get().await;
        let mut cat = Catalog::default();
        for row in timed(pool, client.query(TYPES_SQL, &[])).await? {
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
        for row in timed(pool, client.query(ENUMS_SQL, &[])).await? {
            cat.enums.entry(row.get(0)).or_default().push(row.get(1));
        }
        for labels in cat.enums.values_mut() {
            labels.sort();
        }

        let settings = timed(pool, client.query_one(SETTINGS_SQL, &[])).await?;
        cat.timezone = settings.get(0);
        cat.search_path = settings.get(1);

        for row in timed(pool, client.query(COLUMNS_SQL, &[])).await? {
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
                        hooks_insert: false,
                        hooks_update: false,
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
            if row.get::<_, bool>(10) {
                cat.declared_not_null.insert((oid, attnum));
            }
            if let Some(expr) = row.get::<_, Option<String>>(11) {
                cat.defaults.insert((oid, attnum), expr);
            }
        }
        for row in timed(pool, client.query(KEYS_SQL, &[])).await? {
            let oid: u32 = row.get(0);
            let columns: Vec<i16> = row.get(2);
            if first_char(row.get::<_, &str>(1)) == 'p' {
                cat.primary_keys.insert(oid, columns);
            } else if let Some(ref_columns) = row.get::<_, Option<Vec<i16>>>(4) {
                cat.foreign_keys
                    .entry(oid)
                    .or_default()
                    .push(ForeignKeyRow {
                        columns,
                        ref_table: row.get(3),
                        ref_columns,
                    });
            }
        }
        let by_oid: HashMap<u32, usize> = cat
            .tables
            .iter()
            .enumerate()
            .map(|(i, t)| (t.oid, i))
            .collect();
        for row in timed(pool, client.query(ROW_HOOKS_SQL, &[])).await? {
            if let Some(&i) = by_oid.get(&row.get::<_, u32>(0)) {
                cat.tables[i].hooks_insert = row.get(1);
                cat.tables[i].hooks_update = row.get(2);
            }
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

    /// A catalog of just `tables` (each `(schema, name, columns)`), for unit tests.
    #[cfg(test)]
    pub(crate) fn for_tests(tables: &[(&str, &str, &[&str])], search_path: &[&str]) -> Catalog {
        let mut cat = Catalog {
            search_path: search_path.iter().map(|s| s.to_string()).collect(),
            ..Catalog::default()
        };
        for (i, (schema, name, columns)) in tables.iter().enumerate() {
            let oid = 1000 + i as u32;
            cat.by_name
                .insert((schema.to_string(), name.to_string()), i);
            let mut cols = Vec::new();
            for (n, c) in columns.iter().enumerate() {
                cat.columns.insert((oid, n as i16 + 1), (i, n));
                cols.push(CatalogColumn {
                    schema: schema.to_string(),
                    table: name.to_string(),
                    name: c.to_string(),
                    not_null: false,
                    has_default: false,
                    db_type: cat.type_ref(0, -1),
                });
            }
            cat.tables.push(CatalogTable {
                oid,
                schema: schema.to_string(),
                name: name.to_string(),
                kind: 'r',
                columns: cols,
                hooks_insert: false,
                hooks_update: false,
            });
        }
        cat
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
