//! Turning Postgres errors into Queryfirst diagnostics (QF010, QF011) with did-you-mean.

use std::fmt;

use qf_sqlfile::{Diagnostic, LexedQuery, Severity};

use crate::catalog::Catalog;
use crate::connect::describe_pg_error;

/// Why describing a statement failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PgQueryError {
    /// Postgres rejected the statement.
    Db {
        /// SQLSTATE, e.g. `42703`.
        code: String,
        message: String,
        detail: Option<String>,
        hint: Option<String>,
        /// 1-based character position in the statement.
        position: Option<u32>,
    },
    /// The connection failed or was lost.
    Connection(String),
}

impl fmt::Display for PgQueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PgQueryError::Db { code, message, .. } => write!(f, "{message} (SQLSTATE {code})"),
            PgQueryError::Connection(m) => write!(f, "connection error: {m}"),
        }
    }
}

impl std::error::Error for PgQueryError {}

impl From<tokio_postgres::Error> for PgQueryError {
    fn from(e: tokio_postgres::Error) -> Self {
        match e.as_db_error() {
            Some(db) => PgQueryError::Db {
                code: db.code().code().to_string(),
                message: db.message().to_string(),
                detail: db.detail().map(str::to_string),
                hint: db.hint().map(str::to_string),
                position: match db.position() {
                    Some(tokio_postgres::error::ErrorPosition::Original(p)) => Some(*p),
                    _ => None,
                },
            },
            None => PgQueryError::Connection(describe_pg_error(&e)),
        }
    }
}

/// The error is about the connection, not the statement: report `QF090` and stop.
pub fn is_connection_error(err: &PgQueryError) -> bool {
    match err {
        PgQueryError::Connection(_) => true,
        // 08: connection exception; 57P01-03: shutdown / cannot connect now; 53300: too many
        // connections.
        PgQueryError::Db { code, .. } => {
            code.starts_with("08") || code.starts_with("57P") || code == "53300"
        }
    }
}

/// Build the diagnostic for a describe failure, positioned in the source file.
pub fn to_diagnostic(err: &PgQueryError, lexed: &LexedQuery, catalog: &Catalog) -> Diagnostic {
    let (code, message, detail, hint, position) = match err {
        PgQueryError::Db {
            code,
            message,
            detail,
            hint,
            position,
        } => (code.as_str(), message.as_str(), detail, hint, *position),
        PgQueryError::Connection(m) => {
            let (line, col) = lexed.map.db_char_pos_to_source(1);
            return Diagnostic {
                code: "QF090",
                severity: Severity::Error,
                message: format!("lost the database connection: {m}"),
                hint: None,
                line,
                col,
            };
        }
    };

    if code == "42P18"
        && let Some(n) = param_number(message)
    {
        return undetermined_param(n, lexed, position);
    }

    let mut hint = join_hint(detail.as_deref(), hint.as_deref());
    let message = match code {
        "42703" => match undefined_column(message, lexed, catalog) {
            Some(m) => {
                // Our did-you-mean replaces Postgres' "Perhaps you meant ..." hint.
                if m.contains("did you mean") {
                    hint = detail.clone();
                }
                m
            }
            None => message.to_string(),
        },
        "42P01" => undefined_relation(message, catalog).unwrap_or_else(|| message.to_string()),
        _ => message.to_string(),
    };
    let (line, col) = lexed.map.db_char_pos_to_source(position.unwrap_or(1));
    Diagnostic {
        code: "QF010",
        severity: Severity::Error,
        message,
        hint,
        line,
        col,
    }
}

fn join_hint(detail: Option<&str>, hint: Option<&str>) -> Option<String> {
    match (detail, hint) {
        (Some(d), Some(h)) => Some(format!("{d}\n{h}")),
        (Some(x), None) | (None, Some(x)) => Some(x.to_string()),
        (None, None) => None,
    }
}

/// The `n` in "... parameter $n".
fn param_number(message: &str) -> Option<usize> {
    let at = message.find("parameter $")? + "parameter $".len();
    let digits: String = message[at..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

fn undetermined_param(n: usize, lexed: &LexedQuery, position: Option<u32>) -> Diagnostic {
    let bound = lexed.param_order.get(n.wrapping_sub(1));
    let pos = position.or_else(|| placeholder_char_pos(&lexed.db_statement, n));
    let (line, col) = lexed.map.db_char_pos_to_source(pos.unwrap_or(1));
    let (message, hint) = match bound {
        Some(name) => match name.split_once('.') {
            None => (
                format!("could not determine the type of parameter :{name}"),
                format!("add a cast, e.g. :{name}::text"),
            ),
            Some((param, key)) => (
                format!("could not determine the type of key `{key}` of parameter :{param}"),
                format!(
                    "use :{param} where the type of `{key}` can be inferred, \
                     or cast it in the query, e.g. :{param}::text"
                ),
            ),
        },
        None => (
            format!("could not determine the type of parameter ${n}"),
            "add a cast to the parameter, e.g. :p::text".to_string(),
        ),
    };
    Diagnostic {
        code: "QF011",
        severity: Severity::Error,
        message,
        hint: Some(hint),
        line,
        col,
    }
}

/// 1-based char position of the `$n` placeholder token in the statement.
fn placeholder_char_pos(statement: &str, n: usize) -> Option<u32> {
    let wanted = format!("${n}");
    let tokens = pg_query::scan_tokens(statement).ok()?;
    let param = pg_query::protobuf::Token::Param as i32;
    let tok = tokens.iter().find(|t| {
        t.token == param && statement.get(t.start as usize..t.end as usize) == Some(&wanted)
    })?;
    Some(statement[..tok.start as usize].chars().count() as u32 + 1)
}

/// Text between the first pair of double quotes, or after `prefix` up to the next space.
fn quoted_name(message: &str, prefix: &str) -> Option<String> {
    let rest = message.strip_prefix(prefix)?;
    if let Some(r) = rest.strip_prefix('"') {
        return r.find('"').map(|e| r[..e].to_string());
    }
    // `column c.emial does not exist`: drop the qualifier.
    let word = rest.split_whitespace().next()?;
    Some(word.rsplit('.').next().unwrap_or(word).to_string())
}

/// Best candidate within Levenshtein distance 2 (ties: first in `candidates`).
fn closest<'a>(wanted: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    candidates
        .into_iter()
        .map(|c| (strsim::levenshtein(wanted, c), c))
        .filter(|(d, c)| *d <= 2 && *c != wanted)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

/// Tables referenced by the statement, resolved in the catalog, in a stable order.
fn referenced_tables<'c>(statement: &str, catalog: &'c Catalog) -> Vec<&'c crate::CatalogTable> {
    let Ok(parsed) = pg_query::parse(statement, 0) else {
        return Vec::new();
    };
    let mut names = parsed.tables();
    names.sort();
    names
        .iter()
        .filter_map(|n| match n.split_once('.') {
            Some((s, t)) => catalog.table_ref(Some(s), t),
            None => catalog.table_ref(None, n),
        })
        .collect()
}

fn undefined_column(message: &str, lexed: &LexedQuery, catalog: &Catalog) -> Option<String> {
    let column = quoted_name(message, "column ")?;
    let tables = referenced_tables(&lexed.db_statement, catalog);
    let mut best: Option<(usize, &str, &str)> = None;
    for t in &tables {
        if let Some(c) = closest(&column, t.columns.iter().map(|c| c.name.as_str())) {
            let d = strsim::levenshtein(&column, c);
            if best.is_none_or(|(bd, _, _)| d < bd) {
                best = Some((d, c, &t.name));
            }
        }
    }
    match (best, tables.as_slice()) {
        (Some((_, col, table)), _) => Some(format!(
            r#"column "{column}" does not exist on "{table}"; did you mean "{col}"?"#
        )),
        (None, [only]) => Some(format!(
            r#"column "{column}" does not exist on "{}""#,
            only.name
        )),
        _ => None,
    }
}

fn undefined_relation(message: &str, catalog: &Catalog) -> Option<String> {
    let wanted = quoted_name(message, "relation ")?;
    let (schema, name) = match wanted.split_once('.') {
        Some((s, n)) => (Some(s.to_string()), n.to_string()),
        None => (None, wanted.clone()),
    };
    let candidates = catalog.table_names(schema.as_deref());
    let found = closest(&name, candidates.iter().map(String::as_str))?;
    Some(format!(
        r#"relation "{wanted}" does not exist; did you mean "{found}"?"#
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_param_number() {
        assert_eq!(
            param_number("could not determine data type of parameter $12"),
            Some(12)
        );
        assert_eq!(param_number("syntax error"), None);
    }

    #[test]
    fn finds_placeholder() {
        assert_eq!(placeholder_char_pos("SELECT 'é$1', $1", 1), Some(15));
        assert_eq!(placeholder_char_pos("SELECT $10, $1", 1), Some(13));
    }

    #[test]
    fn quoted_names() {
        assert_eq!(
            quoted_name(r#"column "emial" does not exist"#, "column ").as_deref(),
            Some("emial")
        );
        assert_eq!(
            quoted_name("column c.emial does not exist", "column ").as_deref(),
            Some("emial")
        );
    }

    #[test]
    fn closest_is_within_two() {
        assert_eq!(closest("emial", ["id", "email", "name"]), Some("email"));
        assert_eq!(closest("zzzzz", ["id", "email"]), None);
    }

    #[test]
    fn connection_classes() {
        let db = |code: &str| PgQueryError::Db {
            code: code.into(),
            message: String::new(),
            detail: None,
            hint: None,
            position: None,
        };
        assert!(is_connection_error(&PgQueryError::Connection("x".into())));
        assert!(is_connection_error(&db("08006")));
        assert!(is_connection_error(&db("57P01")));
        assert!(!is_connection_error(&db("42703")));
    }
}
