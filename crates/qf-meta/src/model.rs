use serde::{Deserialize, Serialize};

pub const META_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryMeta {
    pub version: u32,
    /// POSIX, project-relative path of the query file.
    pub source: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `sha256:<hex>` of the normalised (LF) source.
    pub sql_hash: String,
    pub provider: Provider,
    pub statement: Statement,
    pub params: Vec<Param>,
    pub columns: Vec<Column>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<Returns>,
    pub status: Status,
    pub generated: Vec<GeneratedFile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Postgres,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Statement {
    /// Body as pgtyped's `statement` (with `:name` markers).
    pub source: String,
    /// Body with `$n` placeholders, as sent to the database.
    pub db: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Param {
    pub name: String,
    pub db_index: Vec<u32>,
    pub db_type: TypeRef,
    pub required: bool,
    pub required_reason: RequiredReason,
    pub transform: Transform,
    pub locs: Vec<Loc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_override: Option<TypeOverride>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RequiredReason {
    #[serde(rename = "annotation")]
    Annotation,
    #[serde(rename = "inferred:insert-notnull")]
    InferredInsertNotnull,
    #[serde(rename = "inferred:where-eq-notnull")]
    InferredWhereEqNotnull,
    #[serde(rename = "default-nullable")]
    DefaultNullable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Transform {
    Scalar,
    Spread,
    Pick { keys: Vec<PickKeyMeta> },
    PickSpread { keys: Vec<PickKeyMeta> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PickKeyMeta {
    pub name: String,
    pub required: bool,
    pub db_index: u32,
    pub db_type: TypeRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Column {
    /// Name with any override suffix stripped.
    pub name: String,
    pub db_type: TypeRef,
    pub nullable: bool,
    pub nullable_reason: NullableReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ColumnSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_override: Option<TypeOverride>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NullableReason {
    ProvenNotnull,
    OuterJoin,
    Expression,
    AnalyzerGaveUp,
    Override,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnSource {
    pub schema: String,
    pub table: String,
    pub column: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeRef {
    pub schema: String,
    /// pg typname, e.g. `int4`, `accessLevel`.
    pub name: String,
    pub array: bool,
    /// Type modifier text such as `(12,2)` or `(30)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modifier: Option<String>,
    pub category: TypeCategory,
    /// Enum labels, sorted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TypeCategory {
    Base,
    Enum,
    Domain,
    Composite,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeOverride {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    pub array: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum Status {
    Ok,
    Broken { diagnostics: Vec<MetaDiagnostic> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedFile {
    pub path: String,
    pub hash: String,
}

/// Interval in UTF-16 code units of the statement, both ends inclusive (as pgtyped's `locs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Loc {
    pub a: u32,
    pub b: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Returns {
    One,
    Maybe,
    Many,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Note,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetaDiagnostic {
    pub code: String,
    pub severity: Severity,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// 1-based.
    pub line: u32,
    /// 1-based.
    pub col: u32,
}
