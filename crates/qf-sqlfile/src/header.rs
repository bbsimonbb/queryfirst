use crate::name::name_from_path;
use crate::*;
use std::path::Path;

const TAGS: [&str; 4] = ["@name", "@param", "@type", "@returns"];

struct Ctx<'a> {
    src: &'a str,
    diags: Vec<Diagnostic>,
}

impl Ctx<'_> {
    fn pos(&self, offset: usize) -> (u32, u32) {
        let before = &self.src[..offset];
        let line = before.matches('\n').count() as u32 + 1;
        let col = before.rsplit('\n').next().unwrap_or("").chars().count() as u32 + 1;
        (line, col)
    }

    fn err(&mut self, code: &'static str, offset: usize, message: String, hint: Option<&str>) {
        let (line, col) = self.pos(offset);
        self.diags.push(Diagnostic {
            code,
            severity: Severity::Error,
            message,
            hint: hint.map(str::to_string),
            line,
            col,
        });
    }
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

/// Parse a query file (already passed through [`normalise`]).
pub fn parse_query_file(path: &Path, normalised: &str) -> Result<QueryFile, Vec<Diagnostic>> {
    let src = normalised;
    let mut cx = Ctx {
        src,
        diags: Vec::new(),
    };

    let skip_ws = |at: usize| at + (src[at..].len() - src[at..].trim_start().len());
    let mut header: Option<(usize, usize)> = None; // inner range of the comment
    let mut body_start = 0;
    // The header is the first leading comment carrying a tag. Plain comments before it
    // (pgtyped accepts them) join the description; without a header they stay in the body.
    let mut preamble: Vec<&str> = Vec::new();
    let mut start = skip_ws(0);
    let mut unterminated = None;
    loop {
        if src[start..].starts_with("--") {
            let end = src[start..].find('\n').map_or(src.len(), |e| start + e);
            preamble.push(src[start + 2..end].trim());
            start = skip_ws(end);
            continue;
        }
        if !src[start..].starts_with("/*") {
            break;
        }
        let Some(end) = src[start + 2..].find("*/") else {
            unterminated = Some(start);
            break;
        };
        let inner = (start + 2, start + 2 + end);
        let has_tag = src[inner.0..inner.1]
            .lines()
            .any(|l| TAGS.iter().any(|t| strip_line(l).0.starts_with(t)));
        if has_tag {
            header = Some(inner);
            body_start = inner.1 + 2;
            break;
        }
        preamble.extend(src[inner.0..inner.1].split('\n').map(|l| strip_line(l).0));
        start = skip_ws(inner.1 + 2);
    }
    if header.is_none() {
        preamble.clear();
    }
    if let Some(start) = unterminated
        && src[start..].lines().any(|l| {
            TAGS.iter()
                .any(|t| strip_line(l.trim_start_matches("/*")).0.starts_with(t))
        })
    {
        cx.err(
            "QF001",
            start,
            "Unterminated header comment".into(),
            Some("Close the header with `*/`"),
        );
    }

    let mut name: Option<String> = None;
    let mut name_at = 0;
    let mut returns = None;
    let mut expansions = Vec::new();
    let mut type_overrides = Vec::new();
    let mut desc: Vec<&str> = preamble;

    if let Some((a, b)) = header {
        let mut off = a;
        for raw in src[a..b].split('\n') {
            let line_off = off;
            off += raw.len() + 1;
            let (text, skipped) = strip_line(raw);
            let at = line_off + skipped;
            let Some(tag) = TAGS.iter().find(|t| {
                text.starts_with(**t)
                    && text[t.len()..]
                        .chars()
                        .next()
                        .is_none_or(char::is_whitespace)
            }) else {
                desc.push(text);
                continue;
            };
            let rest = text[tag.len()..].trim();
            let rest_at = at + tag.len();
            match *tag {
                "@name" => {
                    if !is_ident(rest) {
                        cx.err("QF001", at, format!("Invalid @name '{rest}'"), None);
                    } else if name.is_some() {
                        cx.err(
                            "QF002",
                            at,
                            "A Queryfirst file must contain exactly one query".into(),
                            Some("Split the file so each query has its own file"),
                        );
                    } else {
                        name = Some(rest.to_string());
                        name_at = at;
                    }
                }
                "@returns" => match rest {
                    "one" => returns = Some(Returns::One),
                    "maybe" => returns = Some(Returns::Maybe),
                    "many" => returns = Some(Returns::Many),
                    "none" => returns = Some(Returns::None),
                    _ => cx.err(
                        "QF001",
                        rest_at,
                        format!("Invalid @returns value '{rest}'"),
                        Some("Use one of: one, maybe, many, none"),
                    ),
                },
                "@param" => match parse_param(rest) {
                    Some(e) => expansions.push(e),
                    None => cx.err(
                        "QF001",
                        at,
                        format!("Invalid @param annotation '{rest}'"),
                        Some("Expected `@param name -> (...)`, `(a, b!)` or `((a, b)...)`"),
                    ),
                },
                _ => {
                    let line = cx.pos(at).0;
                    match parse_type(rest, line) {
                        Some(t) => type_overrides.push(t),
                        None => cx.err(
                            "QF001",
                            at,
                            format!("Invalid @type annotation '{rest}'"),
                            Some("Expected `@type col -> Type from \"module\"`"),
                        ),
                    }
                }
            }
        }
    }

    // Body: everything after the header, trimmed, without trailing ';'.
    let after = &src[body_start..];
    let lead = after.len() - after.trim_start().len();
    let body_offset = body_start + lead;
    let body = src[body_offset..]
        .trim_end_matches(|c: char| c.is_whitespace() || c == ';')
        .to_string();

    if scan(&body).first_code.is_none() {
        cx.err(
            "QF001",
            0,
            "no SQL statement in the file".into(),
            Some("Write the query after the header comment"),
        );
    } else {
        check_single_statement(&mut cx, &body, body_offset);
    }

    if !cx.diags.is_empty() {
        return Err(cx.diags);
    }

    let from_annotation = name.is_some();
    let derived = name_from_path(path);
    let name = name.unwrap_or_else(|| derived.clone());
    let (body_line, body_col) = cx.pos(body_offset);
    let mut diagnostics = Vec::new();
    if from_annotation && name != derived && !derived.is_empty() {
        let (line, col) = cx.pos(name_at);
        diagnostics.push(Diagnostic {
            code: "QF003",
            severity: Severity::Warning,
            message: format!("@name '{name}' does not match the file name (expected '{derived}')"),
            hint: Some("Rename the file or remove the @name annotation".into()),
            line,
            col,
        });
    }

    let description = {
        let joined = desc.join("\n");
        let t = joined.trim();
        (!t.is_empty()).then(|| t.to_string())
    };

    Ok(QueryFile {
        name,
        name_from_annotation: from_annotation,
        description,
        expansions,
        type_overrides,
        returns,
        body,
        body_offset,
        body_line,
        body_col,
        diagnostics,
    })
}

/// Trim a header line: leading whitespace and `*` decoration. Returns (text, bytes skipped).
fn strip_line(raw: &str) -> (&str, usize) {
    let t = raw.trim_start_matches(|c: char| c.is_whitespace() || c == '*');
    let skipped = raw.len() - t.len();
    (t.trim_end(), skipped)
}

fn parse_keys(s: &str) -> Option<Vec<PickKey>> {
    let mut keys = Vec::new();
    let mut parts: Vec<&str> = s.split(',').collect();
    if parts.last().is_some_and(|p| p.is_empty()) {
        parts.pop(); // trailing comma
    }
    for p in parts {
        let (n, required) = match p.strip_suffix('!') {
            Some(n) => (n, true),
            None => (p, false),
        };
        if !is_ident(n) {
            return None;
        }
        keys.push(PickKey {
            name: n.to_string(),
            required,
        });
    }
    (!keys.is_empty()).then_some(keys)
}

fn parse_param(rest: &str) -> Option<Expansion> {
    let (param, tr) = rest.split_once("->")?;
    let param = param.trim();
    if !is_ident(param) {
        return None;
    }
    let tr: String = tr.chars().filter(|c| !c.is_whitespace()).collect();
    let kind = if tr == "(...)" {
        ExpansionKind::Spread
    } else if let Some(inner) = tr.strip_prefix("((").and_then(|t| t.strip_suffix(")...)")) {
        ExpansionKind::PickSpread(parse_keys(inner)?)
    } else {
        let inner = tr.strip_prefix('(')?.strip_suffix(')')?;
        ExpansionKind::Pick(parse_keys(inner)?)
    };
    Some(Expansion {
        param: param.to_string(),
        kind,
    })
}

fn parse_type(rest: &str, line: u32) -> Option<TypeOverrideDecl> {
    let (target, tr) = rest.split_once("->")?;
    let target = target.trim();
    let target = match target.strip_prefix(':') {
        Some(p) if is_ident(p) => OverrideTarget::Param(p.to_string()),
        Some(_) => return None,
        None if is_ident(target) => OverrideTarget::Column(target.to_string()),
        None => return None,
    };
    let tr = tr.trim();
    let (ty, tail) = match tr.find(char::is_whitespace) {
        Some(i) => (&tr[..i], tr[i..].trim()),
        None => (tr, ""),
    };
    let (type_name, array) = match ty.strip_suffix("[]") {
        Some(t) => (t, true),
        None => (ty, false),
    };
    if type_name.is_empty()
        || !type_name
            .chars()
            .all(|c| c.is_alphanumeric() || "_$.".contains(c))
    {
        return None;
    }
    let module = if tail.is_empty() {
        None
    } else {
        let m = tail.strip_prefix("from")?.trim();
        let q = m.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let inner = m[1..].strip_suffix(q)?;
        Some(inner.to_string())
    };
    Some(TypeOverrideDecl {
        target,
        type_name: type_name.to_string(),
        array,
        module,
        line,
    })
}

/// What [`scan`] found; offsets into the scanned SQL.
struct Scan {
    /// The first code: anything but whitespace and comments.
    first_code: Option<usize>,
    /// The first code after a top-level `;`.
    code_after: Option<usize>,
    /// A block comment containing `@name`.
    name_comment: Option<usize>,
}

/// Scan SQL (string and comment aware).
fn scan(sql: &str) -> Scan {
    let b = sql.as_bytes();
    let (mut i, mut seen_semi) = (0, false);
    let (mut first_code, mut code_after, mut name_comment) = (None, None, None);
    while i < b.len() {
        let c = b[i];
        if c == b'-' && b.get(i + 1) == Some(&b'-') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let end = sql[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
            if name_comment.is_none() && sql[i..end].contains("@name") {
                name_comment = Some(i);
            }
            i = end;
            continue;
        }
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if first_code.is_none() && c != b';' {
            first_code = Some(i);
        }
        if seen_semi && code_after.is_none() {
            code_after = Some(i);
        }
        match c {
            b';' => seen_semi = true,
            b'\'' | b'"' => {
                i += 1;
                while i < b.len() && b[i] != c {
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    Scan {
        first_code,
        code_after,
        name_comment,
    }
}

fn check_single_statement(cx: &mut Ctx, body: &str, body_offset: usize) {
    let Scan {
        code_after,
        name_comment,
        ..
    } = scan(body);
    let mut at = name_comment;
    if at.is_none() {
        at = match pg_query::split_with_parser(body) {
            Ok(stmts) => stmts
                .get(1)
                .map(|s| s.as_ptr() as usize - body.as_ptr() as usize)
                .map(|o| o + (body[o..].len() - body[o..].trim_start().len())),
            // Not parseable by Postgres (e.g. `:name` params): use the lexer-based splitter.
            Err(_) => match pg_query::split_with_scanner(body) {
                Ok(stmts) => stmts
                    .get(1)
                    .map(|s| s.as_ptr() as usize - body.as_ptr() as usize)
                    .map(|o| o + (body[o..].len() - body[o..].trim_start().len())),
                Err(_) => code_after,
            },
        };
    }
    if let Some(o) = at {
        cx.err(
            "QF002",
            body_offset + o,
            "A Queryfirst file must contain exactly one query".into(),
            Some("Split the file so each query has its own file"),
        );
    }
}
