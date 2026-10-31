//! Describe a statement: prepare it (Parse/Describe only, never Execute) and read the parameter
//! and result types.

use qf_meta::{TypeCategory, TypeRef};
use tokio_postgres::types::{Kind, Type};

use crate::connect::Pool;
use crate::errors::PgQueryError;
use crate::format_modifier;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Described {
    /// Parameter types, by `$n` (index 0 is `$1`).
    pub params: Vec<TypeRef>,
    pub columns: Vec<DescribedColumn>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescribedColumn {
    pub name: String,
    pub db_type: TypeRef,
    /// The (table oid, attnum) the column comes straight from, if any.
    pub source: Option<(u32, i16)>,
}

/// Prepare `db_statement` on a pooled connection and describe it. The statement is never
/// executed: tokio-postgres sends Parse/Describe/Sync, then Close when the statement drops.
pub async fn describe(pool: &Pool, db_statement: &str) -> Result<Described, PgQueryError> {
    let client = pool.get().await;
    let stmt = client.prepare(db_statement).await?;
    let params = stmt.params().iter().map(|t| type_ref(t, -1)).collect();
    let columns = stmt
        .columns()
        .iter()
        .map(|c| DescribedColumn {
            name: c.name().to_string(),
            db_type: type_ref(c.type_(), c.type_modifier()),
            source: match (c.table_oid(), c.column_id()) {
                (Some(oid), Some(attnum)) if oid != 0 && attnum > 0 => Some((oid, attnum)),
                _ => None,
            },
        })
        .collect();
    Ok(Described { params, columns })
}

/// The [`TypeRef`] for a type as tokio-postgres resolved it; matches
/// [`crate::Catalog::type_ref`] for the same type.
fn type_ref(ty: &Type, typmod: i32) -> TypeRef {
    let category = match ty.kind() {
        Kind::Array(elem) => {
            return TypeRef {
                array: true,
                ..type_ref(elem, typmod)
            };
        }
        Kind::Simple => TypeCategory::Base,
        Kind::Enum(_) => TypeCategory::Enum,
        Kind::Domain(_) => TypeCategory::Domain,
        Kind::Composite(_) => TypeCategory::Composite,
        _ => TypeCategory::Other,
    };
    let enum_values = match ty.kind() {
        Kind::Enum(labels) => {
            let mut v = labels.clone();
            v.sort();
            Some(v)
        }
        _ => None,
    };
    TypeRef {
        schema: ty.schema().to_string(),
        name: ty.name().to_string(),
        array: false,
        modifier: format_modifier(ty.name(), typmod),
        category,
        enum_values,
    }
}
