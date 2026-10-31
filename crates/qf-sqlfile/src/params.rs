//! `:param` lexer, ported from pgtyped's `SQLLexer.g4`, plus the DB statement and source map.

use crate::ir::{Transform, ordered_params};
use crate::sourcemap::SourceMap;
use crate::{Diagnostic, QueryFile, Severity};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    None,
    /// `:p!`
    Bang,
    /// `:p?`
    Question,
}

/// An interval in UTF-16 code units of the statement, both ends inclusive (as pgtyped's `locs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loc {
    pub a: u32,
    pub b: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamUse {
    pub name: String,
    pub marker: Marker,
    /// From the `:` to the last char of the name or marker.
    pub loc: Loc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexedQuery {
    /// Every `:param` use, in source order.
    pub uses: Vec<ParamUse>,
    /// The statement sent to Postgres: `:p` replaced by `$n` as `@pgtyped/runtime` does when
    /// preparing without values (spread `($n)`, pick and pick-spread `($n,$m,...)`).
    pub db_statement: String,
    /// The name bound to each `$n`, in order: `p` for scalars and spreads, `p.key` for picks.
    pub param_order: Vec<String>,
    pub map: SourceMap,
    /// The pgtyped IR `statement`: the body with block comments blanked, up to the final `;`.
    pub statement: String,
}

/// A use, in body bytes: `[start, end)`.
#[derive(Debug, Clone, Copy)]
struct Span {
    start: usize,
    end: usize,
}

struct Scan {
    uses: Vec<(Span, String, Marker)>,
    comments: Vec<Span>,
    /// End of the last token before the statement's `;` (or the end of the body).
    end: usize,
    /// The statement ends inside a line comment that the body trimmed of its `\n`.
    newline: bool,
}

/// Per body byte (at char boundaries): UTF-16 offset and source (line, col).
struct Positions {
    u16: Vec<u32>,
    lc: Vec<(u32, u32)>,
}

impl Positions {
    fn new(q: &QueryFile) -> Self {
        let n = q.body.len();
        let (mut u16, mut lc) = (vec![0; n + 1], vec![(0, 0); n + 1]);
        let (mut u, mut line, mut col) = (0u32, q.body_line, q.body_col);
        for (i, c) in q.body.char_indices() {
            u16[i] = u;
            lc[i] = (line, col);
            u += c.len_utf16() as u32;
            if c == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        u16[n] = u;
        lc[n] = (line, col);
        Positions { u16, lc }
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn scan(body: &str, pos: &Positions) -> Result<Scan, Vec<Diagnostic>> {
    let b = body.as_bytes();
    let err = |at: usize, message: &str, hint: &str| {
        let (line, col) = pos.lc[at];
        vec![Diagnostic {
            code: "QF001",
            severity: Severity::Error,
            message: message.to_string(),
            hint: Some(hint.to_string()),
            line,
            col,
        }]
    };
    let mut s = Scan {
        uses: Vec::new(),
        comments: Vec::new(),
        end: 0,
        newline: false,
    };
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let next = b.get(i + 1).copied();
        let start = i;
        match c {
            b' ' | b'\t' | b'\r' | b'\n' => {
                i += 1;
                continue;
            }
            b';' => break,
            b'-' if next == Some(b'-') => match body[i..].find('\n') {
                Some(e) => i += e + 1,
                None => {
                    i = b.len();
                    s.newline = true;
                }
            },
            b'/' if next == Some(b'*') => {
                let Some(e) = body[i + 2..].find("*/") else {
                    return Err(err(i, "Unterminated block comment", "Close it with `*/`"));
                };
                i += 2 + e + 2;
                s.comments.push(Span { start, end: i });
            }
            b'\'' => {
                // Standard-conforming strings: `''` escapes a quote. Only in E'...' strings (the
                // E not ending a longer identifier) does a backslash escape the next char.
                let escapes = i >= 1
                    && matches!(b[i - 1], b'E' | b'e')
                    && (i < 2 || !(is_word(b[i - 2]) || b[i - 2] == b'$'));
                let mut j = i + 1;
                loop {
                    match b.get(j) {
                        None => {
                            return Err(err(i, "Unterminated string literal", "Close it with `'`"));
                        }
                        Some(b'\\') if escapes => j += 2,
                        Some(b'\'') if b.get(j + 1) == Some(&b'\'') => j += 2,
                        Some(b'\'') => break,
                        Some(_) => j += 1,
                    }
                }
                i = j + 1;
            }
            b'"' => {
                let Some(e) = body[i + 1..].find('"') else {
                    return Err(err(
                        i,
                        "Unterminated quoted identifier",
                        "Close it with `\"`",
                    ));
                };
                i += 1 + e + 1;
            }
            b'$' if i == 0 || !(is_word(b[i - 1]) || b[i - 1] == b'$') => {
                let t = (i + 1..b.len())
                    .find(|&j| !is_word(b[j]))
                    .unwrap_or(b.len());
                if b.get(t) == Some(&b'$') && b.get(i + 1).is_none_or(|&f| !f.is_ascii_digit()) {
                    let tag = &body[i..=t];
                    let Some(e) = body[t + 1..].find(tag) else {
                        return Err(err(
                            i,
                            "Unterminated dollar-quoted string",
                            &format!("Close it with `{tag}`"),
                        ));
                    };
                    i = t + 1 + e + tag.len();
                } else {
                    i += 1;
                }
            }
            b':' if next == Some(b':') => i += 2,
            b':' if next.is_some_and(is_ident_start) => {
                let mut j = i + 1;
                while j < b.len() && is_word(b[j]) {
                    j += 1;
                }
                let name = body[i + 1..j].to_string();
                let after = b.get(j + 1).copied();
                let marker = match b.get(j) {
                    Some(b'!') if after != Some(b'=') => Marker::Bang,
                    Some(b'?') if !matches!(after, Some(b'|' | b'&')) => Marker::Question,
                    _ => Marker::None,
                };
                if marker != Marker::None {
                    j += 1;
                    if matches!(b.get(j), Some(b'!' | b'?')) {
                        return Err(err(
                            i,
                            &format!("Conflicting markers on parameter ':{name}'"),
                            "Use either `!` (required) or `?` (nullable), once",
                        ));
                    }
                }
                s.uses.push((Span { start: i, end: j }, name, marker));
                i = j;
            }
            _ => i += body[i..].chars().next().map_or(1, char::len_utf8),
        }
        s.end = i;
    }
    Ok(s)
}

/// Rebuild the statement from `body[..end]`: block comments blanked, uses replaced by
/// `sub(use index)` when it returns Some. Returns the text and the source point of each char.
fn render(
    q: &QueryFile,
    s: &Scan,
    pos: &Positions,
    sub: &dyn Fn(usize) -> Option<String>,
) -> (String, Vec<(u32, u32)>) {
    enum Ev {
        Comment,
        Use(usize),
    }
    let mut evs: Vec<(Span, Ev)> = s.comments.iter().map(|c| (*c, Ev::Comment)).collect();
    evs.extend(s.uses.iter().enumerate().map(|(k, u)| (u.0, Ev::Use(k))));
    evs.sort_by_key(|e| e.0.start);

    let body = &q.body;
    let (mut out, mut pts) = (String::new(), Vec::new());
    let plain = |out: &mut String, pts: &mut Vec<(u32, u32)>, a: usize, z: usize| {
        for (k, c) in body[a..z].char_indices() {
            out.push(c);
            pts.push(pos.lc[a + k]);
        }
    };
    let mut at = 0;
    for (span, ev) in evs {
        plain(&mut out, &mut pts, at, span.start);
        let text = match ev {
            Ev::Comment => " ".repeat((pos.u16[span.end] - pos.u16[span.start]) as usize),
            Ev::Use(k) => sub(k).unwrap_or_else(|| body[span.start..span.end].to_string()),
        };
        for c in text.chars() {
            out.push(c);
            pts.push(pos.lc[span.start]);
        }
        at = span.end;
    }
    plain(&mut out, &mut pts, at, s.end);
    if s.newline {
        out.push('\n');
        pts.push(pos.lc[s.end]);
    }
    pts.push(pos.lc[s.end]);
    (out, pts)
}

/// Lex the `:param` uses of a query and build the statement Postgres describes.
pub fn lex_params(q: &QueryFile) -> Result<LexedQuery, Vec<Diagnostic>> {
    let pos = Positions::new(q);
    let s = scan(&q.body, &pos)?;
    let uses: Vec<ParamUse> = s
        .uses
        .iter()
        .map(|(span, name, marker)| ParamUse {
            name: name.clone(),
            marker: *marker,
            loc: Loc {
                a: pos.u16[span.start],
                b: pos.u16[span.end] - 1,
            },
        })
        .collect();

    // Assign `$n` as `processSQLQueryIR` does: params in IR order, keys in declaration order.
    let mut param_order = Vec::new();
    let mut subs: Vec<Option<String>> = vec![None; uses.len()];
    for p in ordered_params(q, &uses) {
        if p.uses.is_empty() {
            continue;
        }
        let mut next = |label: String| {
            param_order.push(label);
            format!("${}", param_order.len())
        };
        let text = match p.transform {
            Transform::Scalar => next(p.name.to_string()),
            Transform::Spread => format!("({})", next(p.name.to_string())),
            Transform::Pick(keys) | Transform::PickSpread(keys) => {
                let subs: Vec<String> = keys
                    .iter()
                    .map(|k| next(format!("{}.{}", p.name, k.name)))
                    .collect();
                format!("({})", subs.join(","))
            }
        };
        for &k in &p.uses {
            subs[k] = Some(text.clone());
        }
    }

    let (statement, _) = render(q, &s, &pos, &|_| None);
    let (db_statement, points) = render(q, &s, &pos, &|k| subs[k].clone());
    Ok(LexedQuery {
        uses,
        db_statement,
        param_order,
        map: SourceMap { points },
        statement,
    })
}
