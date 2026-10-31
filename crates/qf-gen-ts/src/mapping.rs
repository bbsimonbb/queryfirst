//! Port of pgtyped's `DefaultTypeMapping` (packages/cli/src/types.ts).

/// A TypeScript type as pgtyped's `Type` union.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TsType {
    /// `{ name }`
    Plain(String),
    /// `{ name, definition }`, declared as `export type name = definition;`
    Alias { name: String, definition: String },
    /// `{ name, from, aliasOf }`
    Import {
        name: String,
        from: String,
        alias_of: Option<String>,
    },
    /// `{ name, enumValues }`
    Enum { name: String, values: Vec<String> },
}

impl TsType {
    pub fn name(&self) -> &str {
        match self {
            TsType::Plain(n) => n,
            TsType::Alias { name, .. }
            | TsType::Import { name, .. }
            | TsType::Enum { name, .. } => name,
        }
    }

    pub fn definition(&self) -> Option<&str> {
        match self {
            TsType::Alias { definition, .. } => Some(definition),
            _ => None,
        }
    }
}

fn plain(n: &str) -> TsType {
    TsType::Plain(n.into())
}

fn alias(n: &str, d: &str) -> TsType {
    TsType::Alias {
        name: n.into(),
        definition: d.into(),
    }
}

/// pgtyped `getArray`.
pub fn get_array(name: &str, definition: Option<&str>) -> TsType {
    TsType::Alias {
        name: format!("{name}Array"),
        definition: format!("({})[]", definition.unwrap_or(name)),
    }
}

fn number() -> TsType {
    plain("number")
}
fn string() -> TsType {
    plain("string")
}
fn number_or_string() -> TsType {
    alias("NumberOrString", "number | string")
}
fn boolean() -> TsType {
    plain("boolean")
}
fn date() -> TsType {
    plain("Date")
}
fn date_or_string() -> TsType {
    alias("DateOrString", "Date | string")
}
fn json() -> TsType {
    alias(
        "Json",
        "null | boolean | number | string | Json[] | { [key: string]: Json }",
    )
}

/// `(parameter, return)` types for a built-in Postgres type name.
pub fn default_mapping(name: &str) -> Option<(TsType, TsType)> {
    Some(match name {
        "int2" | "int4" | "smallint" | "int" | "real" | "float4" | "float" | "float8"
        | "smallserial" | "serial" => (number(), number()),
        "int8" | "bigint" | "numeric" | "decimal" | "bigserial" => (number_or_string(), string()),
        "uuid" | "text" | "varchar" | "char" | "bpchar" | "citext" | "name" | "inet" | "cidr"
        | "macaddr" | "macaddr8" | "money" | "tsvector" => (string(), string()),
        "bit" | "bool" | "boolean" => (boolean(), boolean()),
        "date" | "timestamp" | "timestamptz" | "time" | "timetz" => (date_or_string(), date()),
        "interval" => (date_or_string(), string()),
        "void" => (plain("undefined"), plain("undefined")),
        "json" | "jsonb" => (json(), json()),
        "bytea" => (plain("Buffer"), plain("Buffer")),
        "point" => (get_array("number", None), get_array("number", None)),
        _ => return None,
    })
}
