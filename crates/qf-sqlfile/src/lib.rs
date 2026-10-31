//! qf-sqlfile: query file header, annotations, name derivation, `:param` lexing and pgtyped IR.

mod header;
mod ir;
mod name;
mod params;
mod sourcemap;

pub use header::parse_query_file;
pub use ir::pgtyped_ir;
pub use name::name_from_path;
pub use params::{LexedQuery, Loc, Marker, ParamUse, lex_params};
pub use sourcemap::SourceMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryFile {
    pub name: String,
    pub name_from_annotation: bool,
    pub description: Option<String>,
    pub expansions: Vec<Expansion>,
    pub type_overrides: Vec<TypeOverrideDecl>,
    pub returns: Option<Returns>,
    /// SQL after the header, LF, trailing ';' and surrounding whitespace removed.
    pub body: String,
    /// Byte offset of `body` in the normalised source.
    pub body_offset: usize,
    /// 1-based line and column (in chars) of `body` in the source.
    pub body_line: u32,
    pub body_col: u32,
    /// Non-fatal diagnostics (warnings) found while parsing, e.g. QF003.
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Returns {
    One,
    Maybe,
    Many,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expansion {
    pub param: String,
    pub kind: ExpansionKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpansionKind {
    Spread,
    Pick(Vec<PickKey>),
    PickSpread(Vec<PickKey>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickKey {
    pub name: String,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeOverrideDecl {
    pub target: OverrideTarget,
    pub type_name: String,
    pub array: bool,
    pub module: Option<String>,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverrideTarget {
    Column(String),
    Param(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub severity: Severity,
    pub message: String,
    pub hint: Option<String>,
    /// 1-based.
    pub line: u32,
    /// 1-based.
    pub col: u32,
}

/// Normalise CRLF and lone CR line endings to LF.
pub fn normalise(source: &str) -> String {
    source.replace("\r\n", "\n").replace('\r', "\n")
}
