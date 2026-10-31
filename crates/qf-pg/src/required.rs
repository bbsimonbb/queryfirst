//! Parameter required inference (spec §8.1 step 4): **nullable unless certain**.
//!
//! A parameter is required when an annotation says so (`:p!`), or when *every* occurrence of its
//! placeholder in the statement is in a qualifying position:
//! - a value of the top-level `INSERT … VALUES` (any row) or the top-level `UPDATE … SET col =`,
//!   for a `NOT NULL` catalog column that has no default;
//! - a side of `<notnull column> = $n` in the top-level `WHERE`, or in the `ON` of an inner join
//!   of the top-level `FROM`, reached only through `AND`. The column must be non-null as the
//!   nullability analyzer sees it, so a column on the nullable side of an outer join never counts.
//!
//! The value may be wrapped in casts (`$n::int`) and nothing else. Occurrences are counted with
//! the SQL scanner, not the tree walk, so a use anywhere the walk does not look (subqueries, CTEs,
//! function arguments, `LIMIT`, `ON CONFLICT DO UPDATE`, …) keeps the parameter nullable.
//!
//! Markers win over inference: `?` on any use makes the parameter nullable, else `!` on any use
//! makes it required. For a pick key (`p.key`), a `?` on `:p` makes every key nullable; a `!` on
//! `:p` says the object is required, not its keys, so the keys are still inferred.

use std::collections::HashMap;

use pg_query::NodeEnum;
use pg_query::protobuf::{self as pb, AExprKind, BoolExprType, JoinType, SetOperation, Token};
use qf_meta::RequiredReason;
use qf_sqlfile::{Marker, ParamUse};

use crate::CatalogColumn;
use crate::nullability::{Analyzer, CatalogView, Scope, node_of, str_of, target_rel};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParamRequirement {
    pub required: bool,
    pub reason: RequiredReason,
}

const NULLABLE: ParamRequirement = ParamRequirement {
    required: false,
    reason: RequiredReason::DefaultNullable,
};

/// Per entry of `param_order` (`$1` first). `uses` are the `:param` uses of the query file.
pub fn infer_required(
    db_statement: &str,
    param_order: &[String],
    uses: &[ParamUse],
    cat: &dyn CatalogView,
) -> Vec<ParamRequirement> {
    let inferred = infer(db_statement, param_order.len(), cat);
    param_order
        .iter()
        .zip(inferred)
        .map(|(label, reason)| {
            annotation(label, uses).unwrap_or(match reason {
                Some(reason) => ParamRequirement {
                    required: true,
                    reason,
                },
                None => NULLABLE,
            })
        })
        .collect()
}

fn annotation(label: &str, uses: &[ParamUse]) -> Option<ParamRequirement> {
    let (base, is_key) = match label.split_once('.') {
        Some((base, _)) => (base, true),
        None => (label, false),
    };
    let markers: Vec<Marker> = uses
        .iter()
        .filter(|u| u.name == base)
        .map(|u| u.marker)
        .collect();
    let required = if markers.contains(&Marker::Question) {
        false
    } else if markers.contains(&Marker::Bang) && !is_key {
        true
    } else {
        return None;
    };
    Some(ParamRequirement {
        required,
        reason: RequiredReason::Annotation,
    })
}

/// Qualifying occurrences by byte location: (placeholder number, why it qualifies).
type Hits = HashMap<i32, (i32, RequiredReason)>;

/// The inferred reason per `$1..=$n`, or `None` when it is not certainly required.
fn infer(sql: &str, n: usize, cat: &dyn CatalogView) -> Vec<Option<RequiredReason>> {
    let none = vec![None; n];
    let Ok(parsed) = pg_query::parse(sql, 0) else {
        return none;
    };
    let [raw] = parsed.protobuf.stmts.as_slice() else {
        return none;
    };
    let Some(node) = raw.stmt.as_ref().and_then(|s| s.node.as_ref()) else {
        return none;
    };
    // Every `$n` token of the statement, wherever it is.
    let Ok(scanned) = pg_query::scan(sql) else {
        return none;
    };
    let mut occurrences: HashMap<usize, Vec<i32>> = HashMap::new();
    for t in scanned
        .tokens
        .iter()
        .filter(|t| t.token == Token::Param as i32)
    {
        let number = usize::try_from(t.start)
            .ok()
            .zip(usize::try_from(t.end).ok())
            .and_then(|(a, z)| sql.get(a + 1..z))
            .and_then(|s| s.parse::<usize>().ok());
        let Some(number) = number else {
            return none;
        };
        occurrences.entry(number).or_default().push(t.start);
    }

    let mut hits = Hits::new();
    statement(&mut Analyzer::new(cat), cat, node, &mut hits);

    (1..=n)
        .map(|k| {
            let mut reason = None;
            for loc in occurrences.get(&k)? {
                match hits.get(loc) {
                    Some(&(number, r)) if usize::try_from(number) == Ok(k) => {
                        // An insert/update target is the more telling reason.
                        if reason != Some(RequiredReason::InferredInsertNotnull) {
                            reason = Some(r);
                        }
                    }
                    _ => return None,
                }
            }
            reason
        })
        .collect()
}

fn statement(a: &mut Analyzer, cat: &dyn CatalogView, node: &NodeEnum, hits: &mut Hits) {
    match node {
        NodeEnum::SelectStmt(s) => {
            // Set operations and bare `VALUES`: nothing qualifies.
            if s.op != SetOperation::SetopNone as i32 || !s.values_lists.is_empty() {
                return;
            }
            a.with_ctes(s.with_clause.as_ref(), |a| {
                let scope = a.scope_of(&s.from_clause);
                filters(a, &scope, &s.from_clause, s.where_clause.as_deref(), hits);
                Some(())
            });
        }
        NodeEnum::UpdateStmt(s) => {
            let Some(relation) = &s.relation else { return };
            a.with_ctes(s.with_clause.as_ref(), |a| {
                let mut scope = a.scope_of(&s.from_clause);
                scope.rels.insert(0, target_rel(relation));
                filters(a, &scope, &s.from_clause, s.where_clause.as_deref(), hits);
                Some(())
            });
            for t in &s.target_list {
                // `SET col = $n` only: not `SET col[1] =`, `SET col.f =` or `SET (a, b) =`.
                let Some(NodeEnum::ResTarget(rt)) = &t.node else {
                    continue;
                };
                if !rt.indirection.is_empty() {
                    continue;
                }
                let col = cat.column_by_name(schema_of(relation), &relation.relname, &rt.name);
                target_value(col.as_ref(), rt.val.as_deref(), hits);
            }
        }
        NodeEnum::DeleteStmt(s) => {
            let Some(relation) = &s.relation else { return };
            a.with_ctes(s.with_clause.as_ref(), |a| {
                let mut scope = a.scope_of(&s.using_clause);
                scope.rels.insert(0, target_rel(relation));
                filters(a, &scope, &s.using_clause, s.where_clause.as_deref(), hits);
                Some(())
            });
        }
        NodeEnum::InsertStmt(s) => insert(cat, s, hits),
        _ => {}
    }
}

fn schema_of(relation: &pb::RangeVar) -> Option<&str> {
    (!relation.schemaname.is_empty()).then_some(relation.schemaname.as_str())
}

/// `INSERT … VALUES`, one or more rows, each value going to its column.
fn insert(cat: &dyn CatalogView, s: &pb::InsertStmt, hits: &mut Hits) {
    let Some(relation) = &s.relation else { return };
    let Some(NodeEnum::SelectStmt(sel)) = node_of(&s.select_stmt) else {
        return; // DEFAULT VALUES
    };
    // `INSERT … SELECT` may insert no rows, and so may `VALUES … LIMIT 0`.
    if sel.values_lists.is_empty()
        || sel.op != SetOperation::SetopNone as i32
        || sel.limit_count.is_some()
        || sel.limit_offset.is_some()
        || sel.with_clause.is_some()
    {
        return;
    }
    let schema = schema_of(relation);
    let explicit = !s.cols.is_empty();
    let cols: Vec<Option<CatalogColumn>> = if explicit {
        s.cols
            .iter()
            .map(|c| match &c.node {
                Some(NodeEnum::ResTarget(rt)) if rt.indirection.is_empty() => {
                    cat.column_by_name(schema, &relation.relname, &rt.name)
                }
                _ => None,
            })
            .collect()
    } else {
        match cat.table_columns(schema, &relation.relname) {
            Some(cols) => cols.into_iter().map(Some).collect(),
            None => return,
        }
    };
    for row in &sel.values_lists {
        let Some(NodeEnum::List(row)) = &row.node else {
            continue;
        };
        // Postgres rejects a row whose length does not match; don't guess.
        if (explicit && row.items.len() != cols.len()) || row.items.len() > cols.len() {
            continue;
        }
        for (item, col) in row.items.iter().zip(&cols) {
            target_value(col.as_ref(), Some(item), hits);
        }
    }
}

/// A value stored into `col`: qualifies when it is a placeholder and `col` is `NOT NULL`
/// without a default.
fn target_value(col: Option<&CatalogColumn>, value: Option<&pb::Node>, hits: &mut Hits) {
    if !col.is_some_and(|c| c.not_null && !c.has_default) {
        return;
    }
    if let Some((number, loc)) = value.and_then(|v| v.node.as_ref()).and_then(param) {
        hits.insert(loc, (number, RequiredReason::InferredInsertNotnull));
    }
}

/// `$n`, possibly cast: (number, location).
fn param(node: &NodeEnum) -> Option<(i32, i32)> {
    match node {
        NodeEnum::ParamRef(p) => Some((p.number, p.location)),
        NodeEnum::TypeCast(tc) => node_of(&tc.arg).and_then(param),
        _ => None,
    }
}

/// The top-level `WHERE` and the `ON` of the inner joins in `from`.
fn filters(
    a: &Analyzer,
    scope: &Scope,
    from: &[pb::Node],
    where_clause: Option<&pb::Node>,
    hits: &mut Hits,
) {
    for item in from {
        join_quals(a, scope, item, hits);
    }
    if let Some(w) = where_clause {
        and_chain(a, scope, w, hits);
    }
}

fn join_quals(a: &Analyzer, scope: &Scope, item: &pb::Node, hits: &mut Hits) {
    let Some(NodeEnum::JoinExpr(j)) = &item.node else {
        return;
    };
    // An aliased join hides its tables from the scope: references into it give up anyway.
    if j.alias.is_some() {
        return;
    }
    for side in [&j.larg, &j.rarg].into_iter().flatten() {
        join_quals(a, scope, side, hits);
    }
    // An outer join's ON only decides which rows match, never whether rows come out.
    if j.jointype == JoinType::JoinInner as i32
        && let Some(q) = j.quals.as_deref()
    {
        and_chain(a, scope, q, hits);
    }
}

/// `<notnull column> = $n` conjuncts, through `AND` only.
fn and_chain(a: &Analyzer, scope: &Scope, node: &pb::Node, hits: &mut Hits) {
    match &node.node {
        Some(NodeEnum::BoolExpr(b)) if b.boolop == BoolExprType::AndExpr as i32 => {
            for arg in &b.args {
                and_chain(a, scope, arg, hits);
            }
        }
        Some(NodeEnum::AExpr(e)) if e.kind == AExprKind::AexprOp as i32 => {
            if !matches!(e.name.as_slice(), [op] if str_of(op) == Some("=")) {
                return;
            }
            for (side, other) in [(&e.lexpr, &e.rexpr), (&e.rexpr, &e.lexpr)] {
                let Some((number, loc)) = node_of(side).and_then(param) else {
                    continue;
                };
                if let Some(NodeEnum::ColumnRef(cr)) = node_of(other)
                    && !a.column_ref(cr, scope).nullable
                {
                    hits.insert(loc, (number, RequiredReason::InferredWhereEqNotnull));
                }
            }
        }
        _ => {}
    }
}
