//! Result-column nullability (spec §8.1 step 3): **nullable unless proven otherwise**.
//!
//! The statement is parsed with libpg_query and walked. A column is non-null only when:
//! - it is a reference to a `NOT NULL` catalog column whose relation is not on the nullable side
//!   of an outer join at any level (subqueries and CTEs carry their columns' nullability out,
//!   and the outer side is applied again where they are joined); or
//! - it is an allowlisted expression whose inputs are all non-null (see [`Analyzer::expr`]).
//!
//! Anything the analyzer does not understand is nullable with [`NullableReason::AnalyzerGaveUp`].
//! Postgres's `source` for a column (table oid, attnum) is never taken as proof: it is reported
//! even for outer-joined columns. It is only used as a veto when it points at a nullable column.

use pg_query::NodeEnum;
use pg_query::protobuf::{
    self as pb, AExprKind, BoolExprType, JoinType, SetOperation, SubLinkType,
};
use qf_meta::NullableReason;

use crate::{Catalog, CatalogColumn, Described};

/// What the analyzer needs from the catalog; implemented by [`Catalog`] and by test fakes.
pub trait CatalogView {
    /// A column of a table or view; without a schema, the first table on the search path.
    fn column_by_name(
        &self,
        schema: Option<&str>,
        table: &str,
        column: &str,
    ) -> Option<CatalogColumn>;
    fn column(&self, table_oid: u32, attnum: i16) -> Option<CatalogColumn>;
    /// All columns of a table in `attnum` order, for `*` expansion and column aliases
    /// (`FROM t AS x(a, b)`). Without it, those give up.
    fn table_columns(&self, _schema: Option<&str>, _table: &str) -> Option<Vec<CatalogColumn>> {
        None
    }
}

impl CatalogView for Catalog {
    fn column_by_name(
        &self,
        schema: Option<&str>,
        table: &str,
        column: &str,
    ) -> Option<CatalogColumn> {
        self.table_ref(schema, table)?
            .columns
            .iter()
            .find(|c| c.name == column)
            .cloned()
    }

    fn column(&self, table_oid: u32, attnum: i16) -> Option<CatalogColumn> {
        Catalog::column(self, table_oid, attnum)
    }

    fn table_columns(&self, schema: Option<&str>, table: &str) -> Option<Vec<CatalogColumn>> {
        Some(self.table_ref(schema, table)?.columns.clone())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnNullability {
    pub nullable: bool,
    pub reason: NullableReason,
}

const NOTNULL: ColumnNullability = ColumnNullability {
    nullable: false,
    reason: NullableReason::ProvenNotnull,
};
const GAVE_UP: ColumnNullability = nullable(NullableReason::AnalyzerGaveUp);
const EXPR: ColumnNullability = nullable(NullableReason::Expression);
const OUTER: ColumnNullability = nullable(NullableReason::OuterJoin);

const fn nullable(reason: NullableReason) -> ColumnNullability {
    ColumnNullability {
        nullable: true,
        reason,
    }
}

/// Which reason to report when several nullable inputs combine: giving up is the most useful to
/// surface (the author may want an override), then the outer join, then a plain expression.
fn rank(r: NullableReason) -> u8 {
    match r {
        NullableReason::AnalyzerGaveUp => 3,
        NullableReason::OuterJoin => 2,
        NullableReason::Expression | NullableReason::Override => 1,
        NullableReason::ProvenNotnull => 0,
    }
}

/// Non-null when every input is (and there is at least one input); otherwise nullable with the
/// highest-ranked reason among the nullable inputs.
fn all_notnull(inputs: impl IntoIterator<Item = ColumnNullability>) -> ColumnNullability {
    let mut any = false;
    let mut worst: Option<NullableReason> = None;
    for c in inputs {
        any = true;
        if c.nullable && worst.is_none_or(|w| rank(c.reason) > rank(w)) {
            worst = Some(c.reason);
        }
    }
    match (any, worst) {
        (false, _) => GAVE_UP,
        (true, None) => NOTNULL,
        (true, Some(r)) => nullable(r),
    }
}

/// Non-null when at least one input is (`COALESCE`).
fn any_notnull(inputs: Vec<ColumnNullability>) -> ColumnNullability {
    if inputs.iter().any(|c| !c.nullable) {
        NOTNULL
    } else {
        all_notnull(inputs)
    }
}

/// Per result column of `db_statement`, in the order of `described.columns`. When the analyzer
/// cannot line its view of the output up with `described` (column count or a known name differs,
/// the statement does not parse, `*` cannot be expanded…), every column gives up.
pub fn analyze_columns(
    db_statement: &str,
    described: &Described,
    cat: &dyn CatalogView,
) -> Vec<ColumnNullability> {
    let gave_up = vec![GAVE_UP; described.columns.len()];
    let Ok(parsed) = pg_query::parse(db_statement, 0) else {
        return gave_up;
    };
    let [raw] = parsed.protobuf.stmts.as_slice() else {
        return gave_up;
    };
    let Some(node) = raw.stmt.as_ref().and_then(|s| s.node.as_ref()) else {
        return gave_up;
    };
    let mut analyzer = Analyzer::new(cat);
    let Some(cols) = analyzer.statement(node) else {
        return gave_up;
    };
    if cols.len() != described.columns.len()
        || cols
            .iter()
            .zip(&described.columns)
            .any(|(c, d)| c.name.as_ref().is_some_and(|n| *n != d.name))
    {
        return gave_up;
    }
    cols.iter()
        .zip(&described.columns)
        .map(|(c, d)| {
            let vetoed = d
                .source
                .and_then(|(oid, attnum)| cat.column(oid, attnum))
                .is_some_and(|col| !col.not_null);
            if !c.n.nullable && vetoed {
                GAVE_UP
            } else {
                c.n
            }
        })
        .collect()
}

/// Alias overrides always win: a result name ending in `!` is non-null, in `?` nullable. The
/// suffix is stripped from `names`. A name that is only `!` or `?` is left alone.
pub fn apply_alias_overrides(cols: &mut [ColumnNullability], names: &mut [String]) {
    for (col, name) in cols.iter_mut().zip(names.iter_mut()) {
        let is_nullable = match name.chars().last() {
            Some('!') => false,
            Some('?') => true,
            _ => continue,
        };
        if name.len() == 1 {
            continue;
        }
        name.pop();
        *col = ColumnNullability {
            nullable: is_nullable,
            reason: NullableReason::Override,
        };
    }
}

/// An output column of a statement or relation. `name` is set only when it is certainly the name
/// Postgres gives the column; unnamed columns can still be reached through `*`.
#[derive(Debug, Clone)]
struct OutCol {
    name: Option<String>,
    n: ColumnNullability,
}

enum RelKind {
    /// A catalog table or view, looked up by name.
    Table {
        schema: Option<String>,
        name: String,
    },
    /// A subquery or CTE whose columns were analyzed.
    Known(Vec<OutCol>),
    /// Anything else (functions, `VALUES`, an aliased join, a recursive CTE…): every reference
    /// into it gives up.
    Opaque,
}

/// A range table entry visible in a `FROM` scope.
pub(crate) struct Rel {
    /// The name references use: the alias, else the relation name; empty when it has neither.
    refname: String,
    /// The schema written in `FROM`, for `schema.table.column` references (unaliased only).
    schema: Option<String>,
    kind: RelKind,
    /// On the nullable side of an outer join, at any depth of the join tree.
    nullable: bool,
}

#[derive(Default)]
pub(crate) struct Scope {
    pub(crate) rels: Vec<Rel>,
    /// A `USING` or `NATURAL` join merges columns, so `*` is not the plain concatenation.
    merged: bool,
    /// A `RETURNING` list, where `old` and `new` are special (PG 18).
    returning: bool,
}

enum Lookup {
    Found(ColumnNullability),
    Missing,
    Ambiguous,
}

pub(crate) struct Analyzer<'a> {
    cat: &'a dyn CatalogView,
    /// CTEs in scope, innermost last; `None` columns for those not analyzed (recursive).
    ctes: Vec<(String, Option<Vec<OutCol>>)>,
}

pub(crate) fn str_of(node: &pb::Node) -> Option<&str> {
    match &node.node {
        Some(NodeEnum::String(s)) => Some(&s.sval),
        _ => None,
    }
}

pub(crate) fn node_of(node: &Option<Box<pb::Node>>) -> Option<&NodeEnum> {
    node.as_deref().and_then(|n| n.node.as_ref())
}

fn non_empty_list(node: &Option<Box<pb::Node>>) -> Option<&Vec<pb::Node>> {
    match node_of(node) {
        Some(NodeEnum::List(l)) if !l.items.is_empty() => Some(&l.items),
        _ => None,
    }
}

fn is_star(node: &pb::Node) -> bool {
    matches!(node.node, Some(NodeEnum::AStar(_)))
}

/// Apply column aliases (`AS s(a, b)`) to the first columns. More aliases than columns: `None`.
fn rename(mut cols: Vec<OutCol>, colnames: &[pb::Node]) -> Option<Vec<OutCol>> {
    if colnames.len() > cols.len() {
        return None;
    }
    for (col, name) in cols.iter_mut().zip(colnames) {
        col.name = Some(str_of(name)?.to_string());
    }
    Some(cols)
}

/// The column name Postgres gives an unaliased expression, where it is certain (FigureColname
/// with "strength" 2: column references, function calls, and casts of those).
fn figure_name(node: &NodeEnum) -> Option<String> {
    match node {
        NodeEnum::ColumnRef(c) => c.fields.last().and_then(str_of).map(str::to_string),
        NodeEnum::FuncCall(f) => f.funcname.last().and_then(str_of).map(str::to_string),
        NodeEnum::TypeCast(t) => node_of(&t.arg).and_then(figure_name),
        _ => None,
    }
}

/// Aggregates that return null over no rows: nullable, and not worth an "analyzer gave up" note.
const NULL_ON_EMPTY_AGGREGATES: &[&str] = &[
    "sum",
    "avg",
    "min",
    "max",
    "array_agg",
    "string_agg",
    "json_agg",
    "jsonb_agg",
    "json_object_agg",
    "jsonb_object_agg",
    "bool_and",
    "bool_or",
    "every",
    "bit_and",
    "bit_or",
    "bit_xor",
    "xmlagg",
];

/// Operators that, on built-in types, never return null for non-null operands.
const COMPARISONS: &[&str] = &["=", "<>", "<", ">", "<=", ">="];
const ARITHMETIC: &[&str] = &["+", "-", "*", "/"];

impl<'a> Analyzer<'a> {
    pub(crate) fn new(cat: &'a dyn CatalogView) -> Self {
        Analyzer {
            cat,
            ctes: Vec::new(),
        }
    }

    /// Output columns of a statement; `None` when they cannot be determined.
    fn statement(&mut self, node: &NodeEnum) -> Option<Vec<OutCol>> {
        match node {
            NodeEnum::SelectStmt(s) => self.select(s),
            NodeEnum::InsertStmt(s) => self.dml(
                s.with_clause.as_ref(),
                s.relation.as_ref()?,
                &[],
                s.returning_clause.as_ref(),
            ),
            NodeEnum::UpdateStmt(s) => self.dml(
                s.with_clause.as_ref(),
                s.relation.as_ref()?,
                &s.from_clause,
                s.returning_clause.as_ref(),
            ),
            NodeEnum::DeleteStmt(s) => self.dml(
                s.with_clause.as_ref(),
                s.relation.as_ref()?,
                &s.using_clause,
                s.returning_clause.as_ref(),
            ),
            _ => None,
        }
    }

    /// Run `body` with the CTEs of `with` in scope.
    pub(crate) fn with_ctes<T>(
        &mut self,
        with: Option<&pb::WithClause>,
        body: impl FnOnce(&mut Self) -> Option<T>,
    ) -> Option<T> {
        let depth = self.ctes.len();
        let mut ok = true;
        if let Some(w) = with {
            for cte in &w.ctes {
                let Some(NodeEnum::CommonTableExpr(c)) = &cte.node else {
                    ok = false;
                    break;
                };
                // A recursive CTE's columns depend on themselves: not analyzed.
                let cols = if w.recursive {
                    None
                } else {
                    node_of(&c.ctequery)
                        .and_then(|q| self.statement(q))
                        .and_then(|cols| rename(cols, &c.aliascolnames))
                };
                self.ctes.push((c.ctename.clone(), cols));
            }
        }
        let out = if ok { body(self) } else { None };
        self.ctes.truncate(depth);
        out
    }

    fn select(&mut self, s: &pb::SelectStmt) -> Option<Vec<OutCol>> {
        self.with_ctes(s.with_clause.as_ref(), |a| {
            if s.op != SetOperation::SetopNone as i32 {
                let set_op = [
                    SetOperation::SetopUnion,
                    SetOperation::SetopIntersect,
                    SetOperation::SetopExcept,
                ]
                .iter()
                .any(|op| *op as i32 == s.op);
                if !set_op {
                    return None;
                }
                let left = a.select(s.larg.as_deref()?)?;
                let right = a.select(s.rarg.as_deref()?)?;
                if left.len() != right.len() {
                    return None;
                }
                // Every row comes from one of the branches: non-null only if non-null in all.
                return Some(
                    left.into_iter()
                        .zip(right)
                        .map(|(l, r)| OutCol {
                            name: l.name,
                            n: if l.n.nullable || r.n.nullable {
                                GAVE_UP
                            } else {
                                NOTNULL
                            },
                        })
                        .collect(),
                );
            }
            if !s.values_lists.is_empty() {
                return None;
            }
            let scope = a.scope_of(&s.from_clause);
            let mut cols = a.targets(&s.target_list, &scope)?;
            // ROLLUP / CUBE / GROUPING SETS null out grouped columns in super-aggregate rows.
            if s.group_clause
                .iter()
                .any(|g| matches!(g.node, Some(NodeEnum::GroupingSet(_))))
            {
                for c in &mut cols {
                    c.n = GAVE_UP;
                }
            }
            Some(cols)
        })
    }

    /// `INSERT` / `UPDATE` / `DELETE … RETURNING`: the target row is a real row of the table, so
    /// its `NOT NULL` columns hold; `FROM` / `USING` items join as in a `SELECT`.
    fn dml(
        &mut self,
        with: Option<&pb::WithClause>,
        relation: &pb::RangeVar,
        from: &[pb::Node],
        returning: Option<&pb::ReturningClause>,
    ) -> Option<Vec<OutCol>> {
        self.with_ctes(with, |a| {
            let Some(ret) = returning else {
                return Some(Vec::new());
            };
            // `RETURNING WITH (OLD AS o, …)` renames the PG 18 old/new rows: not handled.
            if !ret.options.is_empty() {
                return None;
            }
            let mut scope = a.scope_of(from);
            scope.rels.insert(0, target_rel(relation));
            scope.returning = true;
            a.targets(&ret.exprs, &scope)
        })
    }

    pub(crate) fn scope_of(&mut self, items: &[pb::Node]) -> Scope {
        let mut scope = Scope::default();
        for item in items {
            self.add_from_item(item, &mut scope.rels, &mut scope.merged);
        }
        scope
    }

    fn add_from_item(&mut self, item: &pb::Node, rels: &mut Vec<Rel>, merged: &mut bool) {
        let opaque = |refname: String| Rel {
            refname,
            schema: None,
            kind: RelKind::Opaque,
            nullable: false,
        };
        match &item.node {
            Some(NodeEnum::RangeVar(rv)) => rels.push(self.range_var(rv)),
            Some(NodeEnum::RangeSubselect(rs)) => {
                let refname = rs
                    .alias
                    .as_ref()
                    .map(|a| a.aliasname.clone())
                    .unwrap_or_default();
                let colnames = rs.alias.as_ref().map_or(&[][..], |a| &a.colnames[..]);
                let cols = node_of(&rs.subquery)
                    .and_then(|q| self.statement(q))
                    .and_then(|cols| rename(cols, colnames));
                rels.push(Rel {
                    refname,
                    schema: None,
                    kind: cols.map_or(RelKind::Opaque, RelKind::Known),
                    nullable: false,
                });
            }
            Some(NodeEnum::JoinExpr(j)) => {
                let mut left = Vec::new();
                let mut right = Vec::new();
                if let Some(l) = &j.larg {
                    self.add_from_item(l, &mut left, merged);
                }
                if let Some(r) = &j.rarg {
                    self.add_from_item(r, &mut right, merged);
                }
                let (left_nullable, right_nullable) = match j.jointype {
                    t if t == JoinType::JoinInner as i32 => (false, false),
                    t if t == JoinType::JoinLeft as i32 => (false, true),
                    t if t == JoinType::JoinRight as i32 => (true, false),
                    // FULL, and anything unexpected: both sides.
                    _ => (true, true),
                };
                for r in &mut left {
                    r.nullable |= left_nullable;
                }
                for r in &mut right {
                    r.nullable |= right_nullable;
                }
                if j.is_natural || !j.using_clause.is_empty() {
                    *merged = true;
                }
                match &j.alias {
                    // An aliased join hides the tables inside it.
                    Some(alias) => rels.push(opaque(alias.aliasname.clone())),
                    None => {
                        rels.extend(left);
                        rels.extend(right);
                    }
                }
            }
            Some(NodeEnum::RangeFunction(f)) => rels.push(opaque(
                f.alias
                    .as_ref()
                    .map(|a| a.aliasname.clone())
                    .unwrap_or_default(),
            )),
            _ => rels.push(opaque(String::new())),
        }
    }

    fn range_var(&self, rv: &pb::RangeVar) -> Rel {
        let schema = (!rv.schemaname.is_empty()).then(|| rv.schemaname.clone());
        let refname = match &rv.alias {
            Some(a) => a.aliasname.clone(),
            None => rv.relname.clone(),
        };
        let colnames = rv.alias.as_ref().map_or(&[][..], |a| &a.colnames[..]);
        let cte = schema
            .is_none()
            .then(|| self.ctes.iter().rev().find(|(n, _)| *n == rv.relname))
            .flatten();
        let kind = match cte {
            Some((_, cols)) => cols
                .clone()
                .and_then(|c| rename(c, colnames))
                .map_or(RelKind::Opaque, RelKind::Known),
            None if colnames.is_empty() => RelKind::Table {
                schema: schema.clone(),
                name: rv.relname.clone(),
            },
            // Column aliases on a table: materialise its columns, then rename.
            None => self
                .cat
                .table_columns(schema.as_deref(), &rv.relname)
                .map(|cols| cols.into_iter().map(catalog_out_col).collect())
                .and_then(|c| rename(c, colnames))
                .map_or(RelKind::Opaque, RelKind::Known),
        };
        Rel {
            refname,
            schema: rv.alias.is_none().then_some(schema).flatten(),
            kind,
            nullable: false,
        }
    }

    fn targets(&self, list: &[pb::Node], scope: &Scope) -> Option<Vec<OutCol>> {
        let mut out = Vec::new();
        for t in list {
            let Some(NodeEnum::ResTarget(rt)) = &t.node else {
                return None;
            };
            let val = node_of(&rt.val)?;
            match val {
                NodeEnum::ColumnRef(cr) if cr.fields.last().is_some_and(is_star) => {
                    out.extend(self.star(cr, scope)?);
                    continue;
                }
                // `(composite).*` expands to an unknown number of columns.
                NodeEnum::AIndirection(ind) if ind.indirection.iter().any(is_star) => {
                    return None;
                }
                _ => {}
            }
            let name = if rt.name.is_empty() {
                figure_name(val)
            } else {
                Some(rt.name.clone())
            };
            out.push(OutCol {
                name,
                n: self.expr(val, scope),
            });
        }
        Some(out)
    }

    /// Expand `*`, `t.*` or `s.t.*`.
    fn star(&self, cr: &pb::ColumnRef, scope: &Scope) -> Option<Vec<OutCol>> {
        let quals: Vec<&str> = cr.fields[..cr.fields.len() - 1]
            .iter()
            .map(str_of)
            .collect::<Option<_>>()?;
        let rels: Vec<&Rel> = match quals.as_slice() {
            [] if scope.merged => return None,
            [] => scope.rels.iter().collect(),
            [table] => vec![self.qualified_rel(scope, None, table)?],
            [schema, table] => vec![self.qualified_rel(scope, Some(schema), table)?],
            _ => return None,
        };
        let mut out = Vec::new();
        for rel in rels {
            let cols: Vec<OutCol> = match &rel.kind {
                RelKind::Table { schema, name } => self
                    .cat
                    .table_columns(schema.as_deref(), name)?
                    .into_iter()
                    .map(catalog_out_col)
                    .collect(),
                RelKind::Known(cols) => cols.clone(),
                RelKind::Opaque => return None,
            };
            out.extend(cols.into_iter().map(|c| OutCol {
                n: if rel.nullable { OUTER } else { c.n },
                ..c
            }));
        }
        Some(out)
    }

    /// The single relation a qualified reference names, if there is exactly one.
    fn qualified_rel<'s>(
        &self,
        scope: &'s Scope,
        schema: Option<&str>,
        table: &str,
    ) -> Option<&'s Rel> {
        if scope.returning && schema.is_none() && (table == "old" || table == "new") {
            return None;
        }
        let mut matches = scope.rels.iter().filter(|r| {
            r.refname == table && schema.is_none_or(|s| r.schema.as_deref() == Some(s))
        });
        let rel = matches.next()?;
        matches.next().is_none().then_some(rel)
    }

    fn lookup(&self, rel: &Rel, column: &str) -> Lookup {
        let found = match &rel.kind {
            RelKind::Table { schema, name } => {
                match self.cat.column_by_name(schema.as_deref(), name, column) {
                    Some(c) => catalog_nullability(&c),
                    None => return Lookup::Missing,
                }
            }
            RelKind::Known(cols) => {
                let mut matches = cols.iter().filter(|c| c.name.as_deref() == Some(column));
                match (matches.next(), matches.next()) {
                    (None, _) => return Lookup::Missing,
                    (Some(c), None) => c.n,
                    (Some(_), Some(_)) => return Lookup::Ambiguous,
                }
            }
            RelKind::Opaque => return Lookup::Missing,
        };
        Lookup::Found(if rel.nullable { OUTER } else { found })
    }

    pub(crate) fn column_ref(&self, cr: &pb::ColumnRef, scope: &Scope) -> ColumnNullability {
        let Some(fields) = cr.fields.iter().map(str_of).collect::<Option<Vec<_>>>() else {
            return GAVE_UP;
        };
        let (rel, column) = match fields.as_slice() {
            [column] => {
                // Unqualified: it must be found in exactly one relation. (Were it also in an
                // opaque one, Postgres would reject the reference as ambiguous, except for a
                // USING/NATURAL merged column, whose value is then one of the two sides'.)
                let mut found = None;
                for rel in &scope.rels {
                    match self.lookup(rel, column) {
                        Lookup::Missing => {}
                        Lookup::Ambiguous => return GAVE_UP,
                        Lookup::Found(n) if found.is_none() => found = Some(n),
                        Lookup::Found(_) => return GAVE_UP,
                    }
                }
                return found.unwrap_or(GAVE_UP);
            }
            [table, column] => (self.qualified_rel(scope, None, table), column),
            [schema, table, column] => (self.qualified_rel(scope, Some(schema), table), column),
            _ => return GAVE_UP,
        };
        match rel.map(|r| self.lookup(r, column)) {
            Some(Lookup::Found(n)) => n,
            _ => GAVE_UP,
        }
    }

    fn expr_box(&self, node: &Option<Box<pb::Node>>, scope: &Scope) -> ColumnNullability {
        node_of(node).map_or(GAVE_UP, |n| self.expr(n, scope))
    }

    fn expr_node(&self, node: &pb::Node, scope: &Scope) -> ColumnNullability {
        node.node.as_ref().map_or(GAVE_UP, |n| self.expr(n, scope))
    }

    /// The expression allowlist. Everything not listed gives up.
    fn expr(&self, node: &NodeEnum, scope: &Scope) -> ColumnNullability {
        match node {
            NodeEnum::ColumnRef(cr) => self.column_ref(cr, scope),
            NodeEnum::AConst(c) if c.isnull => EXPR,
            NodeEnum::AConst(_) => NOTNULL,
            NodeEnum::ParamRef(_) => EXPR,
            // A cast of a non-null value is non-null.
            NodeEnum::TypeCast(tc) => self.expr_box(&tc.arg, scope),
            NodeEnum::CoalesceExpr(c) => {
                any_notnull(c.args.iter().map(|a| self.expr_node(a, scope)).collect())
            }
            NodeEnum::BoolExpr(b) => {
                let known = [
                    BoolExprType::AndExpr,
                    BoolExprType::OrExpr,
                    BoolExprType::NotExpr,
                ]
                .iter()
                .any(|t| *t as i32 == b.boolop);
                if known {
                    all_notnull(b.args.iter().map(|a| self.expr_node(a, scope)))
                } else {
                    GAVE_UP
                }
            }
            // IS [NOT] NULL, IS [NOT] TRUE/FALSE/UNKNOWN are never null.
            NodeEnum::NullTest(_) | NodeEnum::BooleanTest(_) => NOTNULL,
            NodeEnum::SubLink(s) => {
                if s.sub_link_type == SubLinkType::ExistsSublink as i32
                    || s.sub_link_type == SubLinkType::ArraySublink as i32
                {
                    // EXISTS is true or false; ARRAY(SELECT …) is empty, not null, on no rows.
                    NOTNULL
                } else {
                    // A scalar subquery is null on no rows; IN/ANY/ALL can be null.
                    EXPR
                }
            }
            NodeEnum::CaseExpr(c) => {
                if c.defresult.is_none() {
                    return EXPR; // no ELSE: null when nothing matches
                }
                let results = c.args.iter().map(|w| match &w.node {
                    Some(NodeEnum::CaseWhen(w)) => self.expr_box(&w.result, scope),
                    _ => GAVE_UP,
                });
                all_notnull(results.chain([self.expr_box(&c.defresult, scope)]))
            }
            NodeEnum::AExpr(e) => self.a_expr(e, scope),
            NodeEnum::FuncCall(f) => func_call(f),
            _ => GAVE_UP,
        }
    }

    fn a_expr(&self, e: &pb::AExpr, scope: &Scope) -> ColumnNullability {
        let kind = |k: AExprKind| e.kind == k as i32;
        if kind(AExprKind::AexprDistinct) || kind(AExprKind::AexprNotDistinct) {
            // IS [NOT] DISTINCT FROM is never null.
            return NOTNULL;
        }
        if kind(AExprKind::AexprNullif)
            || kind(AExprKind::AexprOpAny)
            || kind(AExprKind::AexprOpAll)
        {
            return EXPR;
        }
        if kind(AExprKind::AexprOp) {
            let op = match e.name.as_slice() {
                [op] => str_of(op),
                _ => None, // OPERATOR(schema.op)
            };
            let Some(op) = op else { return GAVE_UP };
            let binary = e.lexpr.is_some() && e.rexpr.is_some();
            let unary = e.lexpr.is_none() && e.rexpr.is_some() && (op == "-" || op == "+");
            if !(COMPARISONS.contains(&op) || ARITHMETIC.contains(&op)) || !(binary || unary) {
                return GAVE_UP;
            }
            return all_notnull(
                [&e.lexpr, &e.rexpr]
                    .into_iter()
                    .filter(|x| x.is_some())
                    .map(|x| self.expr_box(x, scope)),
            );
        }
        if kind(AExprKind::AexprLike) || kind(AExprKind::AexprIlike) {
            if e.lexpr.is_none() || e.rexpr.is_none() {
                return GAVE_UP;
            }
            return all_notnull([
                self.expr_box(&e.lexpr, scope),
                self.expr_box(&e.rexpr, scope),
            ]);
        }
        let in_or_between = [
            AExprKind::AexprIn,
            AExprKind::AexprBetween,
            AExprKind::AexprNotBetween,
            AExprKind::AexprBetweenSym,
            AExprKind::AexprNotBetweenSym,
        ]
        .into_iter()
        .any(kind);
        if in_or_between {
            let (Some(items), true) = (non_empty_list(&e.rexpr), e.lexpr.is_some()) else {
                return GAVE_UP;
            };
            return all_notnull(
                std::iter::once(self.expr_box(&e.lexpr, scope))
                    .chain(items.iter().map(|i| self.expr_node(i, scope))),
            );
        }
        GAVE_UP
    }
}

/// The target table of an `INSERT` / `UPDATE` / `DELETE`: a real row of the table, never on the
/// nullable side of a join.
pub(crate) fn target_rel(relation: &pb::RangeVar) -> Rel {
    let schema = (!relation.schemaname.is_empty()).then(|| relation.schemaname.clone());
    Rel {
        refname: match &relation.alias {
            Some(alias) => alias.aliasname.clone(),
            None => relation.relname.clone(),
        },
        schema: relation.alias.is_none().then(|| schema.clone()).flatten(),
        kind: RelKind::Table {
            schema,
            name: relation.relname.clone(),
        },
        nullable: false,
    }
}

fn func_call(f: &pb::FuncCall) -> ColumnNullability {
    let Some(names) = f.funcname.iter().map(str_of).collect::<Option<Vec<_>>>() else {
        return GAVE_UP;
    };
    let name = match names.as_slice() {
        [name] | ["pg_catalog", name] => *name,
        _ => return GAVE_UP,
    };
    if name == "count" {
        // count(*) and count(x) are never null, with or without FILTER, DISTINCT or OVER.
        NOTNULL
    } else if NULL_ON_EMPTY_AGGREGATES.contains(&name) {
        EXPR
    } else {
        GAVE_UP
    }
}

fn catalog_nullability(c: &CatalogColumn) -> ColumnNullability {
    if c.not_null { NOTNULL } else { EXPR }
}

fn catalog_out_col(c: CatalogColumn) -> OutCol {
    OutCol {
        n: catalog_nullability(&c),
        name: Some(c.name),
    }
}
