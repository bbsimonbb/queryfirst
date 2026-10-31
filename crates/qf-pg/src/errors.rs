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
///
/// The server's message follows its `lc_messages` and is often not English, so enrichment never
/// reads its words: it keys on the SQLSTATE and takes the offending name from the statement text
/// at the error position. Codes it does not enrich keep the server's message as it is.
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
    let statement = lexed.db_statement.as_str();

    // 42P18 indeterminate_datatype: a parameter never typed (no position), but also other
    // values, e.g. `ARRAY[]` (positioned there). 42P08 ambiguous_parameter: a parameter whose
    // uses gave conflicting types, or one use none (positioned at that use). Only an error about
    // a parameter becomes QF011: a `$n` at the position or, without a position, in the message.
    if code == "42P18" || code == "42P08" {
        let n = match position {
            Some(p) => param_at(statement, p),
            None => param_in_message(message),
        };
        if let Some(n) = n {
            let server_hint = join_hint(detail.as_deref(), hint.as_deref());
            return param_type_error(n, code == "42P08", server_hint, lexed, position);
        }
    }

    let mut hint = join_hint(detail.as_deref(), hint.as_deref());
    let enriched = match (code, position) {
        ("42703", Some(p)) => undefined_column(statement, p, catalog),
        ("42P01", Some(p)) => undefined_relation(statement, p, catalog),
        _ => None,
    };
    let message = match enriched {
        Some(e) => {
            // Our did-you-mean replaces Postgres' own suggestion (its hint).
            if e.suggested {
                hint = detail.clone();
            }
            e.message
        }
        None => message.to_string(),
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

/// Byte offset of the 1-based character `position` (as Postgres counts) in `statement`.
fn byte_at(statement: &str, position: u32) -> Option<usize> {
    let i = usize::try_from(position.checked_sub(1)?).ok()?;
    statement.char_indices().nth(i).map(|(b, _)| b)
}

/// The leading decimal number of `s`.
fn leading_number(s: &str) -> Option<usize> {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    s[..end].parse().ok()
}

/// The `n` of the `$n` placeholder at `position`.
fn param_at(statement: &str, position: u32) -> Option<usize> {
    leading_number(statement[byte_at(statement, position)?..].strip_prefix('$')?)
}

/// The `n` of the first `$n` in the server's message. Only the placeholder is read: it is a
/// format argument, which every translation keeps as it is.
fn param_in_message(message: &str) -> Option<usize> {
    leading_number(message.split_once('$')?.1)
}

/// QF011 for parameter `$n`. `conflicting` (42P08): its uses gave no single type, and the
/// server's detail (e.g. `text versus integer`) and hint come first in our hint.
fn param_type_error(
    n: usize,
    conflicting: bool,
    server_hint: Option<String>,
    lexed: &LexedQuery,
    position: Option<u32>,
) -> Diagnostic {
    let bound = lexed.param_order.get(n.wrapping_sub(1));
    let pos = position.or_else(|| placeholder_char_pos(&lexed.db_statement, n));
    let (line, col) = lexed.map.db_char_pos_to_source(pos.unwrap_or(1));
    let what = if conflicting {
        "could not infer a single type for"
    } else {
        "could not determine the type of"
    };
    let (message, hint) = match bound {
        Some(name) => match name.split_once('.') {
            None => (
                format!("{what} parameter :{name}"),
                format!("add a cast, e.g. :{name}::text"),
            ),
            Some((param, key)) => (
                format!("{what} key `{key}` of parameter :{param}"),
                format!(
                    "use :{param} where the type of `{key}` can be inferred, or cast it in the \
                     query, e.g. :{param}::text"
                ),
            ),
        },
        None => (
            format!("{what} parameter ${n}"),
            "add a cast to the parameter, e.g. :p::text".to_string(),
        ),
    };
    let hint = match server_hint {
        Some(h) => format!("{h}\n{hint}"),
        None => hint,
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

/// The dotted name (`emial`, `c.emial`, `"Sales".custmers`) at the 1-based char `position` of
/// `statement`, part by part as Postgres resolves it: quotes removed, unquoted parts folded to
/// lower case. `None` if no identifier starts there.
fn name_at(statement: &str, position: u32) -> Option<Vec<String>> {
    let mut rest = &statement[byte_at(statement, position)?..];
    let mut parts = Vec::new();
    loop {
        let (part, after) = identifier(rest)?;
        parts.push(part);
        match after.trim_start().strip_prefix('.') {
            Some(r) => rest = r.trim_start(),
            None => return Some(parts),
        }
    }
}

/// One identifier at the start of `s`, and the text after it.
fn identifier(s: &str) -> Option<(String, &str)> {
    if let Some(body) = s.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = body.char_indices();
        while let Some((i, c)) = chars.next() {
            if c != '"' {
                out.push(c);
            } else if body[i + 1..].starts_with('"') {
                out.push('"');
                chars.next();
            } else {
                return (!out.is_empty()).then_some((out, &body[i + 1..]));
            }
        }
        return None;
    }
    let ident_char = |c: char| c.is_alphanumeric() || c == '_' || c == '$' || !c.is_ascii();
    let first = s.chars().next()?;
    if !(first.is_alphabetic() || first == '_' || !first.is_ascii()) {
        return None;
    }
    let end = s.find(|c: char| !ident_char(c)).unwrap_or(s.len());
    // `U&"…"` is a Unicode-escaped identifier, not `u` (but `emial&1` is `emial`).
    if s[..end].eq_ignore_ascii_case("u") && s[end..].starts_with("&\"") {
        return None;
    }
    Some((s[..end].to_ascii_lowercase(), &s[end..]))
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

/// Relation names the statement uses (`schema.name` or `name`), sorted.
fn statement_tables(statement: &str) -> Vec<String> {
    let Ok(parsed) = pg_query::parse(statement, 0) else {
        return Vec::new();
    };
    let mut names = parsed.tables();
    names.sort();
    names
}

/// Tables referenced by the statement, resolved in the catalog, in a stable order.
fn referenced_tables<'c>(statement: &str, catalog: &'c Catalog) -> Vec<&'c crate::CatalogTable> {
    statement_tables(statement)
        .iter()
        .filter_map(|n| match n.split_once('.') {
            Some((s, t)) => catalog.table_ref(Some(s), t),
            None => catalog.table_ref(None, n),
        })
        .collect()
}

/// An English message in schema terms, replacing the server's.
struct Enriched {
    message: String,
    /// It carries a did-you-mean.
    suggested: bool,
}

/// 42703: the column named at `position`, looked up in the tables the statement references.
fn undefined_column(statement: &str, position: u32, catalog: &Catalog) -> Option<Enriched> {
    // `c.emial`: the column is the last part.
    let column = name_at(statement, position)?.pop()?;
    let tables = referenced_tables(statement, catalog);
    let mut best: Option<(usize, &str, &str)> = None;
    for t in &tables {
        if let Some(c) = closest(&column, t.columns.iter().map(|c| c.name.as_str())) {
            let d = strsim::levenshtein(&column, c);
            if best.is_none_or(|(bd, _, _)| d < bd) {
                best = Some((d, c, &t.name));
            }
        }
    }
    let (message, suggested) = match (best, tables.as_slice()) {
        (Some((_, col, table)), _) => (
            format!(r#"column "{column}" does not exist on "{table}"; did you mean "{col}"?"#),
            true,
        ),
        (None, [only]) => (
            format!(r#"column "{column}" does not exist on "{}""#, only.name),
            false,
        ),
        (None, _) => (format!(r#"column "{column}" does not exist"#), false),
    };
    Some(Enriched { message, suggested })
}

/// 42P01: the relation named at `position`, with the closest table in its schema (or, unqualified,
/// on the search path) as did-you-mean.
fn undefined_relation(statement: &str, position: u32, catalog: &Catalog) -> Option<Enriched> {
    let parts = name_at(statement, position)?;
    let (schema, name) = match parts.as_slice() {
        [name] => (None, name.as_str()),
        // `db.schema.table`: the database part is Postgres' business.
        [.., schema, name] => (Some(schema.as_str()), name.as_str()),
        [] => return None,
    };
    let wanted = match schema {
        Some(s) => format!("{s}.{name}"),
        None => name.to_string(),
    };
    // 42P01 is also "missing FROM-clause entry", positioned at a column reference: only a name
    // the statement uses as a relation is a missing relation.
    if !statement_tables(statement).contains(&wanted) {
        return None;
    }
    let candidates = catalog.table_names(schema);
    Some(match closest(name, candidates.iter().map(String::as_str)) {
        Some(found) => Enriched {
            message: format!(r#"relation "{wanted}" does not exist; did you mean "{found}"?"#),
            suggested: true,
        },
        None => Enriched {
            message: format!(r#"relation "{wanted}" does not exist"#),
            suggested: false,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use qf_sqlfile::{lex_params, normalise, parse_query_file};
    use std::path::Path;

    fn lex(src: &str) -> LexedQuery {
        let q = parse_query_file(Path::new("q.sql"), &normalise(src)).unwrap();
        lex_params(&q).unwrap()
    }

    fn catalog() -> Catalog {
        Catalog::for_tests(
            &[
                ("app", "customers", &["id", "email", "name"]),
                ("app", "invoices", &["id", "customer_id", "total"]),
                ("other", "customers", &["id", "email"]),
                ("other", "orders", &["id"]),
            ],
            &["app"],
        )
    }

    /// A server error as a server with French `lc_messages` sends it.
    fn db(code: &str, message: &str, hint: Option<&str>, position: Option<u32>) -> PgQueryError {
        PgQueryError::Db {
            code: code.into(),
            message: message.into(),
            detail: None,
            hint: hint.map(str::to_string),
            position,
        }
    }

    /// 1-based char position of `needle` in `hay`.
    fn pos(hay: &str, needle: &str) -> u32 {
        hay[..hay.find(needle).unwrap()].chars().count() as u32 + 1
    }

    #[test]
    fn undefined_column_from_a_french_server() {
        let l = lex("SELECT id, emial FROM customers");
        let p = pos(&l.db_statement, "emial");
        let e = db(
            "42703",
            "la colonne « emial » n'existe pas",
            Some("Peut-être que vous souhaitiez référencer la colonne « customers.email »."),
            Some(p),
        );
        let d = to_diagnostic(&e, &l, &catalog());
        assert_eq!(d.code, "QF010");
        assert_eq!(
            d.message,
            r#"column "emial" does not exist on "customers"; did you mean "email"?"#
        );
        assert_eq!(d.hint, None, "the server's suggestion is replaced");
        assert_eq!((d.line, d.col), (1, 12));
    }

    #[test]
    fn undefined_column_qualified_quoted_and_accented() {
        let cat = catalog();
        let msg = "la colonne c.emial n'existe pas";
        let l = lex("SELECT 'é', c.emial FROM customers c");
        let d = to_diagnostic(
            &db("42703", msg, None, Some(pos(&l.db_statement, "c.emial"))),
            &l,
            &cat,
        );
        assert_eq!(
            d.message,
            r#"column "emial" does not exist on "customers"; did you mean "email"?"#
        );
        assert_eq!((d.line, d.col), (1, 13));

        // Quoted: the case is kept, and "Email" is one edit from email.
        let l = lex(r#"SELECT "Email" FROM customers"#);
        let d = to_diagnostic(&db("42703", msg, None, Some(8)), &l, &cat);
        assert_eq!(
            d.message,
            r#"column "Email" does not exist on "customers"; did you mean "email"?"#
        );

        // Unquoted names fold to lower case; nothing close on the one table.
        let l = lex("SELECT ZZZZZ FROM invoices");
        let d = to_diagnostic(&db("42703", msg, None, Some(8)), &l, &cat);
        assert_eq!(d.message, r#"column "zzzzz" does not exist on "invoices""#);

        // Two tables, nothing close.
        let l = lex("SELECT zzzzz FROM customers, invoices");
        let d = to_diagnostic(&db("42703", msg, None, Some(8)), &l, &cat);
        assert_eq!(d.message, r#"column "zzzzz" does not exist"#);
    }

    #[test]
    fn unenriched_errors_keep_the_server_message() {
        let cat = catalog();
        let l = lex("SELECT id, emial FROM customers");
        // No position: nothing to read the name from.
        let d = to_diagnostic(
            &db("42703", "la colonne « emial » n'existe pas", None, None),
            &l,
            &cat,
        );
        assert_eq!(d.message, "la colonne « emial » n'existe pas");
        // A code we do not enrich.
        let d = to_diagnostic(
            &db(
                "42601",
                "erreur de syntaxe sur ou près de « FROM »",
                Some("astuce"),
                Some(1),
            ),
            &l,
            &cat,
        );
        assert_eq!(
            (d.code, d.message.as_str(), d.hint.as_deref()),
            (
                "QF010",
                "erreur de syntaxe sur ou près de « FROM »",
                Some("astuce")
            )
        );
    }

    #[test]
    fn undefined_relation_from_a_french_server() {
        let cat = catalog();
        let msg = "la relation « custmers » n'existe pas";
        let l = lex("SELECT id FROM custmers");
        let d = to_diagnostic(&db("42P01", msg, None, Some(16)), &l, &cat);
        assert_eq!(
            d.message,
            r#"relation "custmers" does not exist; did you mean "customers"?"#
        );
        assert_eq!((d.line, d.col), (1, 16));

        // Qualified: candidates come from that schema only.
        let l = lex("SELECT id FROM other.ordrs");
        let d = to_diagnostic(&db("42P01", msg, None, Some(16)), &l, &cat);
        assert_eq!(
            d.message,
            r#"relation "other.ordrs" does not exist; did you mean "orders"?"#
        );
        // Unqualified: only the search path (other.orders is not on it).
        let l = lex("SELECT id FROM ordrs");
        let d = to_diagnostic(&db("42P01", msg, None, Some(16)), &l, &cat);
        assert_eq!(d.message, r#"relation "ordrs" does not exist"#);

        // "missing FROM-clause entry for table x" is 42P01 too, positioned at a column reference.
        let missing = "entrée manquante de la clause FROM pour la table « x »";
        let l = lex("SELECT x.id FROM customers");
        let d = to_diagnostic(&db("42P01", missing, None, Some(8)), &l, &cat);
        assert_eq!(d.message, missing);
    }

    #[test]
    fn undetermined_param_from_a_french_server() {
        let cat = catalog();
        let msg = "n'a pas pu déterminer le type de données du paramètre $1";
        let l = lex("SELECT id FROM customers WHERE :p IS NULL OR email = :p");
        // 42P08, positioned at the use Postgres could not type.
        let d = to_diagnostic(&db("42P08", msg, None, Some(32)), &l, &cat);
        assert_eq!(d.code, "QF011");
        assert_eq!(d.message, "could not infer a single type for parameter :p");
        assert_eq!(d.hint.as_deref(), Some("add a cast, e.g. :p::text"));
        assert_eq!((d.line, d.col), (1, 32));

        // 42P18 has no position: the first placeholder of the parameter its message names.
        let l = lex("SELECT 1 WHERE :a::int = 1 AND :b IS NULL");
        let d = to_diagnostic(&db("42P18", "paramètre $2", None, None), &l, &cat);
        assert_eq!(d.code, "QF011");
        assert_eq!(d.message, "could not determine the type of parameter :b");
        assert_eq!((d.line, d.col), (1, 32));

        // No parameter named anywhere: not QF011; the server's message and hint stay.
        let d = to_diagnostic(&db("42P18", "???", Some("astuce"), None), &l, &cat);
        assert_eq!(
            (d.code, d.message.as_str(), d.hint.as_deref()),
            ("QF010", "???", Some("astuce"))
        );
    }

    #[test]
    fn empty_array_is_not_a_parameter_error() {
        let l = lex("SELECT ARRAY[] AS a WHERE :p::int = 1");
        let msg = "ne peut pas déterminer le type d'un tableau vide";
        let hint = "Convertit explicitement vers le type désiré. Par exemple ARRAY[]::integer[].";
        // Positioned at `ARRAY`, and the message's `$`-free text names no parameter either way.
        let d = to_diagnostic(&db("42P18", msg, Some(hint), Some(8)), &l, &catalog());
        assert_eq!(
            (d.code, d.message.as_str(), d.hint.as_deref()),
            ("QF010", msg, Some(hint))
        );
        assert_eq!((d.line, d.col), (1, 8));
    }

    #[test]
    fn conflicting_param_types_keep_the_server_detail() {
        let l = lex("SELECT 1 WHERE :p = 'a' AND :p = 1");
        let e = PgQueryError::Db {
            code: "42P08".into(),
            message: "types incohérents déduits pour le paramètre $1".into(),
            detail: Some("text versus integer".into()),
            hint: None,
            position: Some(pos(&l.db_statement, "$1 = 1")),
        };
        let d = to_diagnostic(&e, &l, &catalog());
        assert_eq!(d.code, "QF011");
        assert_eq!(d.message, "could not infer a single type for parameter :p");
        assert_eq!(
            d.hint.as_deref(),
            Some("text versus integer\nadd a cast, e.g. :p::text")
        );
        assert_eq!((d.line, d.col), (1, 29));
    }

    #[test]
    fn pick_key_hint_reads_as_one_sentence() {
        let l = lex(
            "/* @name q\n   @param c -> (email, name) */\nSELECT 1 FROM customers WHERE (email, name) = :c",
        );
        let d = to_diagnostic(&db("42P18", "paramètre $2", None, None), &l, &catalog());
        assert_eq!(d.code, "QF011");
        assert_eq!(
            d.message,
            "could not determine the type of key `name` of parameter :c"
        );
        let hint = d.hint.unwrap();
        assert_eq!(
            hint,
            "use :c where the type of `name` can be inferred, or cast it in the query, e.g. :c::text"
        );
        assert!(!hint.contains("  "), "{hint:?}");
    }

    #[test]
    fn reads_names_at_positions() {
        let v = |parts: &[&str]| Some(parts.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(name_at("SELECT Emial FROM t", 8), v(&["emial"]));
        assert_eq!(name_at("SELECT c . emial FROM t", 8), v(&["c", "emial"]));
        assert_eq!(name_at(r#"SELECT "Sa""les".t"#, 8), v(&["Sa\"les", "t"]));
        assert_eq!(name_at("SELECT 'é', données FROM t", 13), v(&["données"]));
        assert_eq!(name_at("SELECT (c).x", 8), None);
        assert_eq!(name_at(r#"SELECT U&"x""#, 8), None);
        assert_eq!(name_at(r#"SELECT u&"x""#, 8), None);
        assert_eq!(name_at("SELECT emial&1 FROM t", 8), v(&["emial"]));
        assert_eq!(name_at("SELECT u&1 FROM t", 8), v(&["u"]));
        assert_eq!(name_at("SELECT x", 99), None);
        assert_eq!(name_at("SELECT x", 0), None);
    }

    #[test]
    fn finds_params() {
        assert_eq!(param_at("SELECT 'é', $12", 13), Some(12));
        assert_eq!(param_at("SELECT x", 8), None);
        assert_eq!(
            param_in_message("could not determine data type of parameter $12"),
            Some(12)
        );
        assert_eq!(param_in_message("paramètre $3 inconnu"), Some(3));
        assert_eq!(param_in_message("syntax error"), None);
    }

    #[test]
    fn finds_placeholder() {
        assert_eq!(placeholder_char_pos("SELECT 'é$1', $1", 1), Some(15));
        assert_eq!(placeholder_char_pos("SELECT $10, $1", 1), Some(13));
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
