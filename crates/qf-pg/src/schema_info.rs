//! What `qf schema` shows: the catalog's tables with their declared columns, keys and defaults,
//! as plain data that renders (and tests) without a database.

use std::collections::HashMap;

use qf_meta::TypeRef;

use crate::catalog::Catalog;

/// A column as the database declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaColumn {
    pub name: String,
    pub db_type: TypeRef,
    /// The declared `NOT NULL` (`attnotnull`).
    pub not_null: bool,
    /// `false` when the `NOT NULL` is declared but the analyzer does not rely on it (NOT VALID,
    /// NO INHERIT, a foreign table, or a descendant whose own rows may hold NULL).
    pub not_null_trusted: bool,
    /// The default expression, or `GENERATED ALWAYS AS IDENTITY` / `GENERATED … AS (expr) STORED`.
    pub default: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKey {
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaTable {
    pub schema: String,
    pub name: String,
    /// `relkind`: `r`, `p`, `v`, `m` or `f`.
    pub kind: char,
    pub columns: Vec<SchemaColumn>,
    pub primary_key: Vec<String>,
    pub foreign_keys: Vec<ForeignKey>,
}

impl SchemaTable {
    /// `schema.name`.
    pub fn qualified(&self) -> String {
        format!("{}.{}", self.schema, self.name)
    }

    /// `table`, `view`, `materialized view` or `foreign table`.
    pub fn kind_name(&self) -> &'static str {
        match self.kind {
            'v' => "view",
            'm' => "materialized view",
            'f' => "foreign table",
            _ => "table",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchemaInfo {
    /// Sorted by schema, then name.
    pub tables: Vec<SchemaTable>,
    pub search_path: Vec<String>,
}

impl SchemaInfo {
    /// A table by `name` or `schema.name`. A bare name takes the first match on the search
    /// path, else the only table of that name anywhere.
    pub fn find(&self, arg: &str) -> Option<&SchemaTable> {
        if let Some((schema, name)) = arg.split_once('.') {
            return self
                .tables
                .iter()
                .find(|t| t.schema == schema && t.name == name);
        }
        let named = || self.tables.iter().filter(|t| t.name == arg);
        self.search_path
            .iter()
            .find_map(|s| named().find(|t| &t.schema == s))
            .or_else(|| {
                let mut it = named();
                let first = it.next()?;
                it.next().is_none().then_some(first)
            })
    }

    /// Up to three table names (`schema.name`) within Levenshtein distance 2 of `arg`, closest
    /// first. A bare `arg` is compared with the bare table names.
    pub fn suggest(&self, arg: &str) -> Vec<String> {
        let qualified = arg.contains('.');
        let mut found: Vec<(usize, String)> = self
            .tables
            .iter()
            .filter_map(|t| {
                let full = t.qualified();
                let d = strsim::levenshtein(arg, if qualified { &full } else { &t.name });
                (d <= 2).then_some((d, full))
            })
            .collect();
        found.sort();
        found.into_iter().take(3).map(|(_, n)| n).collect()
    }
}

impl Catalog {
    /// Everything `qf schema` shows, from the single catalog load. `not_null` is the declared
    /// `NOT NULL`; `not_null_trusted` says whether the analyzer relies on it.
    pub fn schema_info(&self) -> SchemaInfo {
        let attnums: HashMap<(u32, &str), i16> = self
            .columns
            .iter()
            .map(|(&(oid, attnum), &(t, c))| {
                ((oid, self.tables[t].columns[c].name.as_str()), attnum)
            })
            .collect();
        let col_names = |oid: u32, nums: &[i16]| -> Vec<String> {
            nums.iter()
                .map(|&n| {
                    self.column(oid, n)
                        .map_or_else(|| format!("#{n}"), |c| c.name)
                })
                .collect()
        };
        let mut tables: Vec<SchemaTable> = self
            .tables
            .iter()
            .map(|t| {
                let columns = t
                    .columns
                    .iter()
                    .map(|c| {
                        let key = attnums.get(&(t.oid, c.name.as_str())).map(|&n| (t.oid, n));
                        SchemaColumn {
                            name: c.name.clone(),
                            db_type: c.db_type.clone(),
                            not_null: c.not_null
                                || key.is_some_and(|k| self.declared_not_null.contains(&k)),
                            not_null_trusted: c.not_null,
                            default: key.and_then(|k| self.defaults.get(&k).cloned()),
                        }
                    })
                    .collect();
                let foreign_keys = self
                    .foreign_keys
                    .get(&t.oid)
                    .into_iter()
                    .flatten()
                    .filter_map(|fk| {
                        let r = self.tables.iter().find(|r| r.oid == fk.ref_table)?;
                        Some(ForeignKey {
                            columns: col_names(t.oid, &fk.columns),
                            ref_schema: r.schema.clone(),
                            ref_table: r.name.clone(),
                            ref_columns: col_names(r.oid, &fk.ref_columns),
                        })
                    })
                    .collect();
                SchemaTable {
                    schema: t.schema.clone(),
                    name: t.name.clone(),
                    kind: t.kind,
                    columns,
                    primary_key: self
                        .primary_keys
                        .get(&t.oid)
                        .map(|k| col_names(t.oid, k))
                        .unwrap_or_default(),
                    foreign_keys,
                }
            })
            .collect();
        tables.sort_by(|a, b| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
        SchemaInfo {
            tables,
            search_path: self.search_path.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(schema: &str, name: &str) -> SchemaTable {
        SchemaTable {
            schema: schema.into(),
            name: name.into(),
            kind: 'r',
            columns: vec![],
            primary_key: vec![],
            foreign_keys: vec![],
        }
    }

    fn info() -> SchemaInfo {
        SchemaInfo {
            tables: vec![
                table("audit", "events"),
                table("audit", "users"),
                table("public", "customers"),
                table("public", "users"),
            ],
            search_path: vec!["public".into()],
        }
    }

    #[test]
    fn find_uses_search_path_then_unique() {
        let i = info();
        assert_eq!(i.find("users").unwrap().schema, "public");
        assert_eq!(i.find("audit.users").unwrap().schema, "audit");
        assert_eq!(i.find("events").unwrap().schema, "audit");
        assert!(i.find("nope").is_none());
        let mut ambiguous = info();
        ambiguous.search_path.clear();
        assert!(ambiguous.find("users").is_none());
    }

    #[test]
    fn suggest_is_close_and_sorted() {
        let i = info();
        assert_eq!(i.suggest("custmers"), vec!["public.customers"]);
        assert_eq!(i.suggest("user"), vec!["audit.users", "public.users"]);
        assert!(i.suggest("zzzzzz").is_empty());
    }
}
