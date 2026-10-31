//! The per-query pipeline (read → describe → analyze → meta → generate) and the write plan.
//!
//! Everything is built in memory first: a connection error anywhere aborts the run before a
//! single file is written.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Component, Path, PathBuf};

use futures_util::StreamExt;
use qf_gen_ts::{OutputFile, PgtypedBinding};
use qf_meta::{
    Column, ColumnSource, GeneratedFile, Loc, META_VERSION, MetaDiagnostic, NullableReason, Param,
    PickKeyMeta, Provider, QueryMeta, RequiredReason, Statement, Status, Transform, TypeCategory,
    TypeOverride, TypeRef, from_json, meta_path_for, sha256_tag, to_canonical_json,
};
use qf_pg::{
    Catalog, ConnError, Described, PgQueryError, Pool, analyze_columns, apply_alias_overrides,
    infer_required, is_connection_error, to_diagnostic,
};
use qf_sqlfile::{
    Diagnostic, Expansion, ExpansionKind, LexedQuery, Marker, OverrideTarget, PickKey, QueryFile,
    Severity, TypeOverrideDecl, lex_params, name_from_path, normalise, parse_query_file,
};

use crate::config::Config;
use crate::discover::{Matcher, walk};
use crate::posix;
use crate::report::ReportedDiagnostic;
use crate::writer::{normalise_text, remove_if_exists, write_if_changed};

/// Describing a statement. [`Pool`] is the real one; tests substitute fakes.
pub trait Describe {
    fn describe(&self, db_statement: &str)
    -> impl Future<Output = Result<Described, PgQueryError>>;
}

impl Describe for Pool {
    fn describe(
        &self,
        db_statement: &str,
    ) -> impl Future<Output = Result<Described, PgQueryError>> {
        qf_pg::describe(self, db_statement)
    }
}

/// One query after describe and analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Analyzed {
    /// Project-relative path of the query file.
    pub source: PathBuf,
    /// The meta to write (`generated` is filled by [`render_outputs`]). A broken query has
    /// status `Broken` and keeps the last good params, columns and statement.
    pub meta: QueryMeta,
    /// Everything to report for this query: errors, warnings and notes.
    pub diagnostics: Vec<Diagnostic>,
    /// The meta on disk before this run, when there was a readable one.
    pub previous: Option<QueryMeta>,
    /// Where the SQL body starts, for diagnostics with no better position.
    pub body_pos: (u32, u32),
}

/// A query's text, for [`analyze_query`].
#[derive(Debug, Clone, Copy)]
pub struct QuerySource<'a> {
    /// Project-relative POSIX path, e.g. `app/orders/[id]/(queries)/get-order.psql`.
    pub source: &'a str,
    /// The whole file through [`normalise`].
    pub normalised: &'a str,
}

fn error(
    code: &'static str,
    (line, col): (u32, u32),
    message: String,
    hint: Option<String>,
) -> Diagnostic {
    Diagnostic {
        code,
        severity: Severity::Error,
        message,
        hint,
        line,
        col,
    }
}

fn unknown_type() -> TypeRef {
    TypeRef {
        schema: "pg_catalog".into(),
        name: "unknown".into(),
        array: false,
        modifier: None,
        category: TypeCategory::Other,
        enum_values: None,
    }
}

/// The `dbType` recorded for a pick parameter itself (its keys carry the real types).
fn record_type() -> TypeRef {
    TypeRef {
        name: "record".into(),
        ..unknown_type()
    }
}

fn meta_returns(r: qf_sqlfile::Returns) -> qf_meta::Returns {
    match r {
        qf_sqlfile::Returns::One => qf_meta::Returns::One,
        qf_sqlfile::Returns::Maybe => qf_meta::Returns::Maybe,
        qf_sqlfile::Returns::Many => qf_meta::Returns::Many,
        qf_sqlfile::Returns::None => qf_meta::Returns::None,
    }
}

fn meta_severity(s: Severity) -> qf_meta::Severity {
    match s {
        Severity::Error => qf_meta::Severity::Error,
        Severity::Warning => qf_meta::Severity::Warning,
        Severity::Note => qf_meta::Severity::Note,
    }
}

fn has_errors(diags: &[Diagnostic]) -> bool {
    diags.iter().any(|d| d.severity == Severity::Error)
}

/// No last good types: no params, no columns and no statement (an empty statement is how the
/// generator and `qf check` tell "nothing to keep" from a last good query with no params and no
/// columns, such as a `DELETE`).
fn clear_last_good(meta: &mut QueryMeta) {
    meta.params.clear();
    meta.columns.clear();
    meta.statement = Statement {
        source: String::new(),
        db: String::new(),
    };
}

/// Mark `meta` broken with the errors in `diags`. Its params, columns and statement become the
/// last good ones from `previous` (all empty when there is none), so the wrapper keeps its types
/// and `qf check` can rebuild it from the meta alone.
fn make_broken(meta: &mut QueryMeta, diags: &[Diagnostic], previous: Option<&QueryMeta>) {
    match previous {
        Some(p) => {
            meta.params = p.params.clone();
            meta.columns = p.columns.clone();
            meta.statement = p.statement.clone();
        }
        None => clear_last_good(meta),
    }
    meta.status = Status::Broken {
        diagnostics: diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| MetaDiagnostic {
                code: d.code.to_string(),
                severity: meta_severity(d.severity),
                message: d.message.clone(),
                hint: d.hint.clone(),
                line: d.line,
                col: d.col,
            })
            .collect(),
    };
}

fn finish(
    src: &QuerySource,
    mut meta: QueryMeta,
    diagnostics: Vec<Diagnostic>,
    previous: Option<&QueryMeta>,
    body_pos: (u32, u32),
) -> Analyzed {
    if has_errors(&diagnostics) {
        make_broken(&mut meta, &diagnostics, previous);
    }
    Analyzed {
        source: PathBuf::from(src.source),
        meta,
        diagnostics,
        previous: previous.cloned(),
        body_pos,
    }
}

pub(crate) fn connection_lost(source: &str, e: &PgQueryError) -> ConnError {
    let detail = match e {
        PgQueryError::Connection(m) => m.clone(),
        PgQueryError::Db { code, message, .. } => format!("{message} (SQLSTATE {code})"),
    };
    ConnError {
        code: "QF090",
        message: format!("lost the database connection while describing {source}"),
        detail: Some(detail),
        hint: None,
    }
}

/// Describe and analyze one parsed query file and assemble its meta (with `generated` empty).
///
/// `previous` is the meta last written for this query, if any: a broken query keeps its params,
/// columns and statement. Only a connection error is an `Err`; every query problem is reported
/// in [`Analyzed::diagnostics`] and makes the meta `Broken`. This is the whole per-query
/// pipeline, usable on an in-memory file (`qf describe`).
pub async fn analyze_query<D: Describe>(
    src: &QuerySource<'_>,
    q: &QueryFile,
    db: &D,
    cat: &Catalog,
    previous: Option<&QueryMeta>,
) -> Result<Analyzed, ConnError> {
    let mut diags = q.diagnostics.clone();
    let body_pos = (q.body_line, q.body_col);
    let mut meta = QueryMeta {
        version: META_VERSION,
        source: src.source.to_string(),
        name: q.name.clone(),
        description: q.description.clone(),
        sql_hash: sha256_tag(src.normalised.as_bytes()),
        provider: Provider::Postgres,
        statement: Statement {
            source: q.body.clone(),
            db: String::new(),
        },
        params: Vec::new(),
        columns: Vec::new(),
        returns: q.returns.map(meta_returns),
        status: Status::Ok,
        generated: Vec::new(),
    };

    let lexed = match lex_params(q) {
        Ok(l) => l,
        Err(d) => {
            diags.extend(d);
            return Ok(finish(src, meta, diags, previous, body_pos));
        }
    };
    meta.statement = Statement {
        source: lexed.statement.clone(),
        db: lexed.db_statement.clone(),
    };
    for e in &q.expansions {
        if !lexed.uses.iter().any(|u| u.name == e.param) {
            diags.push(error(
                "QF004",
                param_decl_pos(src.normalised, &e.param),
                format!("@param {} is declared but never used in the query", e.param),
                Some(format!(
                    "use :{} in the query, or remove the @param line",
                    e.param
                )),
            ));
        }
    }

    let described = match db.describe(&lexed.db_statement).await {
        Ok(d) => d,
        Err(e) if is_connection_error(&e) => return Err(connection_lost(src.source, &e)),
        Err(e) => {
            diags.push(to_diagnostic(&e, &lexed, cat));
            return Ok(finish(src, meta, diags, previous, body_pos));
        }
    };
    assemble(
        q,
        src.normalised,
        &lexed,
        &described,
        cat,
        &mut meta,
        &mut diags,
    );
    Ok(finish(src, meta, diags, previous, body_pos))
}

/// Fill `meta`'s params and columns from what the database `described` for `lexed`, applying
/// the query's own declarations: alias suffixes (`"x!"`, `"x?"`), `:p!` / `:p?`, pick keys'
/// `!` and `@type`. Problems go to `diags`. `false` (with a `QF010`) when the database reported
/// a different number of parameters than the statement has; params and columns stay empty.
///
/// `normalised` is the source text, used only to position `@type` diagnostics.
pub fn assemble(
    q: &QueryFile,
    normalised: &str,
    lexed: &LexedQuery,
    described: &Described,
    cat: &Catalog,
    meta: &mut QueryMeta,
    diags: &mut Vec<Diagnostic>,
) -> bool {
    let body_pos = (q.body_line, q.body_col);
    if described.params.len() != lexed.param_order.len() {
        diags.push(error(
            "QF010",
            body_pos,
            format!(
                "the database reported {} parameters for a statement with {}",
                described.params.len(),
                lexed.param_order.len()
            ),
            None,
        ));
        return false;
    }

    meta.columns = columns(lexed, described, cat, diags, body_pos);
    meta.params = params(q, lexed, described, cat);
    apply_type_overrides(q, normalised, meta, diags);

    let gave_up: Vec<String> = meta
        .columns
        .iter()
        .filter(|c| c.nullable_reason == NullableReason::AnalyzerGaveUp)
        .map(|c| format!("\"{}\"", c.name))
        .collect();
    if let Some(first) = meta
        .columns
        .iter()
        .find(|c| c.nullable_reason == NullableReason::AnalyzerGaveUp)
    {
        let what = if gave_up.len() == 1 {
            format!("column {}", gave_up[0])
        } else {
            format!("columns {}", gave_up.join(", "))
        };
        diags.push(Diagnostic {
            code: "QF020",
            severity: Severity::Note,
            message: format!("could not prove {what} non-null; typed as nullable"),
            hint: Some(format!(
                "if it is never null, alias it with a \"!\" suffix, e.g. AS \"{}!\"",
                first.name
            )),
            line: body_pos.0,
            col: body_pos.1,
        });
    }
    true
}

/// The query file a committed meta was generated from, as far as [`assemble`] needs it: its
/// statement (`statement.source` keeps the `:p!` / `:p?` markers and the alias suffixes) as the
/// body, its `@param` expansions and its `@type` declarations. Used where only the meta exists
/// (`qf test`, possibly from a manifest). Lexing the result gives back the meta's statements.
///
/// A pick key recorded `required` becomes a `key!` declaration: the meta does not tell the
/// author's `!` from inference, so both are kept as the author's.
pub fn query_file_from_meta(meta: &QueryMeta) -> QueryFile {
    let keys = |keys: &[PickKeyMeta]| -> Vec<PickKey> {
        keys.iter()
            .map(|k| PickKey {
                name: k.name.clone(),
                required: k.required,
            })
            .collect()
    };
    let expansions = meta
        .params
        .iter()
        .filter_map(|p| {
            let kind = match &p.transform {
                Transform::Scalar => return None,
                Transform::Spread => ExpansionKind::Spread,
                Transform::Pick { keys: k } => ExpansionKind::Pick(keys(k)),
                Transform::PickSpread { keys: k } => ExpansionKind::PickSpread(keys(k)),
            };
            Some(Expansion {
                param: p.name.clone(),
                kind,
            })
        })
        .collect();
    let decl = |target: OverrideTarget, ov: &TypeOverride| TypeOverrideDecl {
        target,
        type_name: ov.name.clone(),
        array: ov.array,
        module: ov.module.clone(),
        line: 1,
    };
    let type_overrides = meta
        .columns
        .iter()
        .filter_map(|c| {
            let ov = c.type_override.as_ref()?;
            Some(decl(OverrideTarget::Column(c.name.clone()), ov))
        })
        .chain(meta.params.iter().filter_map(|p| {
            let ov = p.type_override.as_ref()?;
            Some(decl(OverrideTarget::Param(p.name.clone()), ov))
        }))
        .collect();
    QueryFile {
        name: meta.name.clone(),
        name_from_annotation: true,
        description: meta.description.clone(),
        expansions,
        type_overrides,
        returns: None,
        body: meta.statement.source.clone(),
        body_offset: 0,
        body_line: 1,
        body_col: 1,
        diagnostics: Vec::new(),
    }
}

fn columns(
    lexed: &LexedQuery,
    described: &Described,
    cat: &Catalog,
    diags: &mut Vec<Diagnostic>,
    body_pos: (u32, u32),
) -> Vec<Column> {
    let mut names: Vec<String> = described.columns.iter().map(|c| c.name.clone()).collect();
    // Postgres's name for an unnamed column; checked before `?` suffixes are stripped.
    let anonymous: Vec<bool> = names.iter().map(|n| n == "?column?").collect();
    let mut nulls = analyze_columns(&lexed.db_statement, described, cat);
    apply_alias_overrides(&mut nulls, &mut names);
    for (i, n) in names.iter().enumerate() {
        if anonymous[i] {
            diags.push(error(
                "QF013",
                body_pos,
                format!("result column {} has no name", i + 1),
                Some("give it an alias, e.g. `AS total`".into()),
            ));
        } else if names[..i]
            .iter()
            .zip(&anonymous)
            .filter(|(m, anon)| !**anon && *m == n)
            .count()
            == 1
        {
            diags.push(error(
                "QF012",
                body_pos,
                format!("duplicate result column name \"{n}\""),
                Some(format!("alias one of them, e.g. `AS other_{n}`")),
            ));
        }
    }
    described
        .columns
        .iter()
        .zip(names)
        .zip(nulls)
        .map(|((d, name), n)| Column {
            name,
            db_type: d.db_type.clone(),
            nullable: n.nullable,
            nullable_reason: n.reason,
            source: d
                .source
                .and_then(|(oid, attnum)| cat.column(oid, attnum))
                .map(|c| ColumnSource {
                    schema: c.schema,
                    table: c.table,
                    column: c.name,
                }),
            type_override: None,
        })
        .collect()
}

/// Params in pgtyped's IR order: `@param` declarations first, then the others by first use.
fn params(q: &QueryFile, lexed: &LexedQuery, described: &Described, cat: &Catalog) -> Vec<Param> {
    let reqs = infer_required(&lexed.db_statement, &lexed.param_order, &lexed.uses, cat);
    let indexes = |label: &str| -> Vec<usize> {
        lexed
            .param_order
            .iter()
            .enumerate()
            .filter(|(_, l)| *l == label)
            .map(|(i, _)| i)
            .collect()
    };
    let ty_at = |i: usize| {
        described
            .params
            .get(i)
            .cloned()
            .unwrap_or_else(unknown_type)
    };

    // Declared but unused expansions are QF004 errors; they get no param.
    let mut order: Vec<(&str, Option<&ExpansionKind>)> = q
        .expansions
        .iter()
        .filter(|e| lexed.uses.iter().any(|u| u.name == e.param))
        .map(|e| (e.param.as_str(), Some(&e.kind)))
        .collect();
    for u in &lexed.uses {
        if !order.iter().any(|(n, _)| *n == u.name) {
            order.push((&u.name, None));
        }
    }

    order
        .into_iter()
        .map(|(name, kind)| {
            let uses = || lexed.uses.iter().filter(|u| u.name == name);
            let locs: Vec<Loc> = uses()
                .map(|u| Loc {
                    a: u.loc.a,
                    b: u.loc.b,
                })
                .collect();
            let base = Param {
                name: name.to_string(),
                db_index: Vec::new(),
                db_type: unknown_type(),
                required: false,
                required_reason: RequiredReason::DefaultNullable,
                transform: Transform::Scalar,
                locs,
                type_override: None,
            };
            match kind {
                None | Some(ExpansionKind::Spread) => {
                    let idx = indexes(name);
                    let first = idx.first().copied();
                    Param {
                        db_index: idx.iter().map(|&i| i as u32 + 1).collect(),
                        db_type: first.map_or_else(unknown_type, ty_at),
                        required: first.is_some_and(|i| reqs[i].required),
                        required_reason: first
                            .map_or(RequiredReason::DefaultNullable, |i| reqs[i].reason),
                        transform: if kind.is_some() {
                            Transform::Spread
                        } else {
                            Transform::Scalar
                        },
                        ..base
                    }
                }
                Some(ExpansionKind::Pick(keys) | ExpansionKind::PickSpread(keys)) => {
                    let keys: Vec<PickKeyMeta> = keys
                        .iter()
                        .map(|k| {
                            let i = indexes(&format!("{name}.{}", k.name)).first().copied();
                            PickKeyMeta {
                                name: k.name.clone(),
                                // The header's `key!` is the author's; else inference.
                                required: k.required || i.is_some_and(|i| reqs[i].required),
                                db_index: i.map_or(0, |i| i as u32 + 1),
                                db_type: i.map_or_else(unknown_type, ty_at),
                            }
                        })
                        .collect();
                    // `:p!` / `:p?` on the object apply to the object (pgtyped semantics).
                    let markers: Vec<Marker> = uses().map(|u| u.marker).collect();
                    let required =
                        markers.contains(&Marker::Bang) && !markers.contains(&Marker::Question);
                    let annotated = required || markers.contains(&Marker::Question);
                    Param {
                        db_type: record_type(),
                        required,
                        required_reason: if annotated {
                            RequiredReason::Annotation
                        } else {
                            RequiredReason::DefaultNullable
                        },
                        transform: if matches!(kind, Some(ExpansionKind::Pick(_))) {
                            Transform::Pick { keys }
                        } else {
                            Transform::PickSpread { keys }
                        },
                        ..base
                    }
                }
            }
        })
        .collect()
}

/// 1-based (line, col) of the `@param <name>` declaration in the source, else (1, 1).
fn param_decl_pos(normalised: &str, name: &str) -> (u32, u32) {
    for (i, line) in normalised.split('\n').enumerate() {
        let mut from = 0;
        while let Some(at) = line[from..].find("@param").map(|a| a + from) {
            let rest = line[at + "@param".len()..].trim_start();
            if let Some(after) = rest.strip_prefix(name)
                && after.trim_start().starts_with("->")
            {
                return (i as u32 + 1, line[..at].chars().count() as u32 + 1);
            }
            from = at + "@param".len();
        }
    }
    (1, 1)
}

/// 1-based column of `@type` on 1-based `line` of the source, else 1.
fn annotation_col(normalised: &str, line: u32) -> u32 {
    normalised
        .split('\n')
        .nth(line.saturating_sub(1) as usize)
        .and_then(|l| l.find("@type").map(|i| l[..i].chars().count() as u32 + 1))
        .unwrap_or(1)
}

fn apply_type_overrides(
    q: &QueryFile,
    normalised: &str,
    meta: &mut QueryMeta,
    diags: &mut Vec<Diagnostic>,
) {
    for decl in &q.type_overrides {
        let pos = (decl.line, annotation_col(normalised, decl.line));
        let ov = TypeOverride {
            name: decl.type_name.clone(),
            module: decl.module.clone(),
            array: decl.array,
        };
        let list = |names: Vec<String>, none: &str, some: &str| {
            if names.is_empty() {
                none.to_string()
            } else {
                format!("{some}: {}", names.join(", "))
            }
        };
        let (slot, what, known) = match &decl.target {
            OverrideTarget::Column(n) => {
                let known = list(
                    meta.columns.iter().map(|c| c.name.clone()).collect(),
                    "the query returns no columns",
                    "the result columns are",
                );
                let slot = meta
                    .columns
                    .iter_mut()
                    .find(|c| c.name == *n)
                    .map(|c| &mut c.type_override);
                (slot, format!("result column \"{n}\""), known)
            }
            OverrideTarget::Param(n) => {
                let known = list(
                    meta.params.iter().map(|p| format!(":{}", p.name)).collect(),
                    "the query has no parameters",
                    "the parameters are",
                );
                let slot = meta
                    .params
                    .iter_mut()
                    .find(|p| p.name == *n)
                    .map(|p| &mut p.type_override);
                (slot, format!("parameter :{n}"), known)
            }
        };
        match slot {
            Some(s) if s.is_none() => *s = Some(ov),
            Some(_) => diags.push(error(
                "QF004",
                pos,
                format!("@type for {what} is declared more than once"),
                Some("keep one @type per column or parameter".into()),
            )),
            None => diags.push(error(
                "QF004",
                pos,
                format!("@type names {what}, which the query does not have"),
                Some(known),
            )),
        }
    }
}

/// A file that could not be read or parsed: broken, keeping what the previous meta knew.
fn broken_unparsed(
    source: &str,
    name_path: &Path,
    normalised: &str,
    diagnostics: Vec<Diagnostic>,
    previous: Option<&QueryMeta>,
) -> Analyzed {
    let mut meta = QueryMeta {
        version: META_VERSION,
        source: source.to_string(),
        name: previous.map_or_else(|| name_from_path(name_path), |p| p.name.clone()),
        description: previous.and_then(|p| p.description.clone()),
        sql_hash: sha256_tag(normalised.as_bytes()),
        provider: Provider::Postgres,
        statement: Statement {
            source: String::new(),
            db: String::new(),
        },
        params: Vec::new(),
        columns: Vec::new(),
        returns: previous.and_then(|p| p.returns),
        status: Status::Ok,
        generated: Vec::new(),
    };
    make_broken(&mut meta, &diagnostics, previous);
    Analyzed {
        source: PathBuf::from(source),
        meta,
        diagnostics,
        previous: previous.cloned(),
        body_pos: (1, 1),
    }
}

fn meta_rel(source: &Path) -> String {
    posix(&meta_path_for(source))
}

fn read_meta(path: &Path) -> Option<QueryMeta> {
    from_json(&std::fs::read_to_string(path).ok()?).ok()
}

/// Read, parse, describe and analyze one query file of the project.
pub async fn analyze_file<D: Describe>(
    project: &Path,
    rel: &Path,
    db: &D,
    cat: &Catalog,
) -> Result<Analyzed, ConnError> {
    let source = posix(rel);
    // A meta at this path written for another source (`q.sql` vs `q.psql`) is not ours.
    let previous = read_meta(&project.join(meta_path_for(rel))).filter(|m| m.source == source);
    analyze_path(&project.join(rel), &source, rel, db, cat, previous.as_ref()).await
}

/// Read the file at `path` and analyze it as [`analyze_text`] does; a file that cannot be read
/// is a broken query (`QF001`).
pub async fn analyze_path<D: Describe>(
    path: &Path,
    source: &str,
    name_path: &Path,
    db: &D,
    cat: &Catalog,
    previous: Option<&QueryMeta>,
) -> Result<Analyzed, ConnError> {
    let unreadable = |message: String| {
        let d = error("QF001", (1, 1), message, None);
        broken_unparsed(source, name_path, "", vec![d], previous)
    };
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return Ok(unreadable(format!("cannot read {source}: {e}"))),
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return Ok(unreadable(format!("{source} is not valid UTF-8")));
    };
    analyze_text(source, name_path, &text, db, cat, previous).await
}

/// Normalise, parse, describe and analyze the text of one query. `source` is what the meta and
/// diagnostics call it; `name_path` is the path its default name comes from (`get-order.sql`
/// becomes `getOrder`). Used for files and for `qf describe`'s virtual `<sql>` file.
pub async fn analyze_text<D: Describe>(
    source: &str,
    name_path: &Path,
    text: &str,
    db: &D,
    cat: &Catalog,
    previous: Option<&QueryMeta>,
) -> Result<Analyzed, ConnError> {
    let normalised = normalise(text);
    let q = match parse_query_file(name_path, &normalised) {
        Ok(q) => q,
        Err(d) => return Ok(broken_unparsed(source, name_path, &normalised, d, previous)),
    };
    let src = QuerySource {
        source,
        normalised: &normalised,
    };
    analyze_query(&src, &q, db, cat, previous).await
}

/// Analyze `files` (project-relative) concurrently, at most `cfg.pool` at a time. Results are in
/// the order of `files`. The first connection error aborts everything still in flight.
pub async fn analyze_all<D: Describe>(
    project: &Path,
    cfg: &Config,
    files: &[PathBuf],
    db: &D,
    cat: &Catalog,
) -> Result<Vec<Analyzed>, ConnError> {
    let mut stream = futures_util::stream::iter(
        files
            .iter()
            .enumerate()
            .map(|(i, f)| async move { (i, analyze_file(project, f, db, cat).await) }),
    )
    .buffer_unordered(cfg.pool.max(1));
    let mut done = Vec::with_capacity(files.len());
    while let Some((i, r)) = stream.next().await {
        // Returning drops the stream, cancelling the describes still running.
        done.push((i, r?));
    }
    done.sort_by_key(|(i, _)| *i);
    Ok(done.into_iter().map(|(_, a)| a).collect())
}

/// Run the generators for `a`, filling `a.meta.generated`. A generator error (e.g. `QF014`)
/// makes the query broken; a broken query whose last good types no longer render falls back to
/// an error-only output.
pub fn render_outputs(a: &mut Analyzed, cfg: &Config) -> Vec<OutputFile> {
    loop {
        let mut outs = Vec::new();
        let mut failed = Vec::new();
        for g in &cfg.generators {
            match qf_gen_ts::generate(&a.meta, None, &g.ts_options(), &PgtypedBinding) {
                Ok(f) => outs.push(f),
                Err(ds) => failed.extend(ds),
            }
        }
        if failed.is_empty() {
            let mut generated: Vec<GeneratedFile> = outs
                .iter()
                .map(|o| GeneratedFile {
                    path: o.path.clone(),
                    hash: sha256_tag(normalise_text(&o.content).as_bytes()),
                })
                .collect();
            generated.sort_by(|x, y| x.path.cmp(&y.path));
            a.meta.generated = generated;
            return outs;
        }
        if a.meta.status == Status::Ok {
            for g in failed {
                a.diagnostics
                    .push(error(g.code, a.body_pos, g.message, None));
            }
            make_broken(&mut a.meta, &a.diagnostics, a.previous.as_ref());
        } else {
            clear_last_good(&mut a.meta);
        }
    }
}

/// Files to write and delete, all project-relative POSIX paths.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// Generated outputs first, then meta files.
    pub writes: Vec<(String, String)>,
    pub deletes: Vec<String>,
}

/// A project-relative path we may delete: plain components only (no `..`, no root or drive).
fn safe_rel(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.contains('\\')
        && Path::new(rel)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

/// How every generated wrapper starts (Queryfirst's and pgtyped's alike).
const GENERATED_HEADER: &str = "/** Types generated for queries found in ";

fn conflict(path: &str, a: &str, b: &str) -> ReportedDiagnostic {
    ReportedDiagnostic::project(
        "QF091",
        format!("{a} and {b} would both write {path}"),
        Some("rename one of the files, or change emitTemplate in queryfirst.json".into()),
    )
}

/// Render every output and decide what to write and delete. Nothing touches the disk.
///
/// Deletes are only ever files recorded in a meta's `generated` list: outputs a query no longer
/// produces, and — on whole-project runs — the outputs and meta of a query whose source file is
/// gone.
pub fn plan(
    project: &Path,
    cfg: &Config,
    analyzed: &mut [Analyzed],
    whole_project: bool,
) -> Result<Plan, ReportedDiagnostic> {
    let sources: BTreeSet<String> = analyzed.iter().map(|a| posix(&a.source)).collect();
    let mut owner: BTreeMap<String, String> = BTreeMap::new();
    let mut outputs = Vec::new();
    let mut metas = Vec::new();
    for a in analyzed.iter_mut() {
        let source = posix(&a.source);
        let files = render_outputs(a, cfg);
        let meta_path = meta_rel(&a.source);
        for (path, content) in files
            .into_iter()
            .map(|f| (f.path, f.content))
            .chain([(meta_path.clone(), to_canonical_json(&a.meta))])
        {
            if sources.contains(&path) {
                return Err(conflict(&path, &source, &path));
            }
            if let Some(other) = owner.insert(path.clone(), source.clone()) {
                return Err(conflict(&path, &other, &source));
            }
            if path == meta_path {
                metas.push((path, content));
            } else {
                outputs.push((path, content));
            }
        }
    }

    // A meta on disk at one of our paths that belongs to another, still existing source: a
    // scoped run would otherwise overwrite that query's meta and wrapper.
    for a in analyzed.iter() {
        let path = meta_rel(&a.source);
        if let Some(other) = read_meta(&project.join(&path))
            && other.source != a.meta.source
            && safe_rel(&other.source)
            && project.join(&other.source).is_file()
        {
            return Err(conflict(&path, &other.source, &a.meta.source));
        }
    }

    // An existing file at an output path that no meta recorded is someone else's (e.g. a
    // hand-written `foo.qf.ts`): never overwrite it. A wrapper pgtyped (or Queryfirst, with
    // its meta lost) generated starts with the shared header, and is ours to replace.
    for a in analyzed.iter() {
        let on_disk = read_meta(&project.join(meta_rel(&a.source)));
        let recorded = |path: &str| {
            [a.previous.as_ref(), on_disk.as_ref()]
                .into_iter()
                .flatten()
                .any(|m| m.generated.iter().any(|g| g.path == path))
        };
        for g in &a.meta.generated {
            let file = project.join(&g.path);
            if !file.is_file() || recorded(&g.path) {
                continue;
            }
            let generated =
                std::fs::read(&file).is_ok_and(|b| b.starts_with(GENERATED_HEADER.as_bytes()));
            if !generated {
                return Err(ReportedDiagnostic::project(
                    "QF091",
                    format!(
                        "{} exists and was not generated by Queryfirst; refusing to overwrite \
                         it with the output of {}",
                        g.path, a.meta.source
                    ),
                    Some(
                        "move or rename the file, or change emitTemplate in queryfirst.json".into(),
                    ),
                ));
            }
        }
    }

    let mut deletes: Vec<String> = Vec::new();
    let delete = |rel: &str, deletes: &mut Vec<String>| {
        if safe_rel(rel)
            && !owner.contains_key(rel)
            && !sources.contains(rel)
            && project.join(rel).is_file()
            && !deletes.iter().any(|d| d == rel)
        {
            deletes.push(rel.to_string());
        }
    };
    // Outputs the previous meta recorded that this run no longer produces.
    for a in analyzed.iter() {
        if let Some(p) = &a.previous
            && p.source == a.meta.source
        {
            for g in &p.generated {
                delete(&g.path, &mut deletes);
            }
        }
    }
    if whole_project {
        let mut found = Vec::new();
        let m = Matcher::new(cfg);
        // An unreadable directory only means fewer orphans found.
        let _ = walk(project, "", &m, &mut |f| {
            if f.ends_with(".qf.json") && !m.excluded(f) {
                found.push(f.to_string());
            }
        });
        for rel in found {
            if owner.contains_key(&rel) {
                continue;
            }
            let Some(meta) = read_meta(&project.join(&rel)) else {
                continue;
            };
            let belongs = safe_rel(&meta.source) && meta_rel(Path::new(&meta.source)) == rel;
            if !belongs || project.join(&meta.source).exists() {
                continue;
            }
            for g in &meta.generated {
                delete(&g.path, &mut deletes);
            }
            delete(&rel, &mut deletes);
        }
    }
    outputs.extend(metas);
    Ok(Plan {
        writes: outputs,
        deletes,
    })
}

/// Carry out `plan`. Returns the files written (changed) and deleted.
pub fn apply(
    project: &Path,
    plan: &Plan,
) -> Result<(Vec<String>, Vec<String>), ReportedDiagnostic> {
    let fail = |what: &str, path: &str, e: std::io::Error| {
        ReportedDiagnostic::project("QF091", format!("cannot {what} {path}: {e}"), None)
    };
    let mut written = Vec::new();
    for (rel, content) in &plan.writes {
        if write_if_changed(&project.join(rel), content).map_err(|e| fail("write", rel, e))? {
            written.push(rel.clone());
        }
    }
    let mut deleted = Vec::new();
    for rel in &plan.deletes {
        if remove_if_exists(&project.join(rel)).map_err(|e| fail("delete", rel, e))? {
            deleted.push(rel.clone());
        }
    }
    Ok((written, deleted))
}
