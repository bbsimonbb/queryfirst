//! qf-pg: the only crate that touches a database.
//!
//! Nothing is ever executed against the user's database: statements are only prepared and
//! described, and the catalog is read with a few `SELECT`s.

mod catalog;
mod connect;
mod describe;
mod errors;
mod nullability;
mod required;

pub use catalog::{Catalog, CatalogColumn, CatalogTable, WriteOp};
pub use connect::{
    ConnError, ConnSource, DEFAULT_QUERY_TIMEOUT, KEEPALIVES_IDLE, Pool, PooledClient,
    READ_ONLY_OPTION, ResolvedConn, SslMode, connect_pool, redact, resolve_connection,
};
pub use describe::{Described, DescribedColumn, describe};
pub use errors::{PgQueryError, is_connection_error, to_diagnostic};
pub use nullability::{CatalogView, ColumnNullability, analyze_columns, apply_alias_overrides};
pub use required::{ParamRequirement, infer_required};

/// Format a Postgres type modifier (`atttypmod`) for the type `name` (the element type for
/// arrays), e.g. `(12,2)` for `numeric`, `(30)` for `varchar`. `None` when there is none.
pub(crate) fn format_modifier(name: &str, typmod: i32) -> Option<String> {
    if typmod < 0 {
        return None;
    }
    match name {
        "numeric" if typmod >= 4 => {
            let t = typmod - 4;
            let precision = (t >> 16) & 0xffff;
            // The scale is an 11-bit signed value in the low bits (negative since PG 15).
            let scale = ((t & 0x7ff) ^ 1024) - 1024;
            Some(format!("({precision},{scale})"))
        }
        "varchar" | "bpchar" if typmod >= 4 => Some(format!("({})", typmod - 4)),
        "bit" | "varbit" | "timestamp" | "timestamptz" | "time" | "timetz" => {
            Some(format!("({typmod})"))
        }
        "interval" => {
            let precision = typmod & 0xffff;
            (precision != 0xffff).then(|| format!("({precision})"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::format_modifier;

    #[test]
    fn modifiers() {
        let numeric = ((12 << 16) | 2) + 4;
        assert_eq!(
            format_modifier("numeric", numeric).as_deref(),
            Some("(12,2)")
        );
        assert_eq!(format_modifier("varchar", 34).as_deref(), Some("(30)"));
        assert_eq!(format_modifier("bpchar", 5).as_deref(), Some("(1)"));
        assert_eq!(format_modifier("timestamptz", 3).as_deref(), Some("(3)"));
        assert_eq!(format_modifier("numeric", -1), None);
        assert_eq!(format_modifier("text", 10), None);
    }
}
