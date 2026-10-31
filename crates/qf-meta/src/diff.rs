//! Semantic diff between a committed meta and what a target database reports.

use crate::model::{
    Column, Param, PickKeyMeta, QueryMeta, Severity, Transform, TypeCategory, TypeRef,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference {
    /// `QF040`..`QF047`.
    pub code: &'static str,
    pub severity: Severity,
    pub message: String,
}

fn fmt_type(t: &TypeRef) -> String {
    let mut s = String::new();
    if t.schema != "pg_catalog" {
        s.push_str(&t.schema);
        s.push('.');
    }
    s.push_str(&t.name);
    if let Some(m) = &t.modifier {
        s.push_str(m);
    }
    if t.array {
        s.push_str("[]");
    }
    s
}

fn same_type(a: &TypeRef, b: &TypeRef) -> bool {
    a.schema == b.schema
        && a.name == b.name
        && a.modifier == b.modifier
        && a.array == b.array
        && a.category == b.category
}

fn category_name(c: TypeCategory) -> &'static str {
    match c {
        TypeCategory::Base => "base",
        TypeCategory::Enum => "enum",
        TypeCategory::Domain => "domain",
        TypeCategory::Composite => "composite",
        TypeCategory::Other => "other",
    }
}

/// Like `fmt_type`, with the category appended when the two sides differ in it.
fn fmt_pair(e: &TypeRef, a: &TypeRef) -> (String, String) {
    if e.category == a.category {
        (fmt_type(e), fmt_type(a))
    } else {
        (
            format!("{} ({})", fmt_type(e), category_name(e.category)),
            format!("{} ({})", fmt_type(a), category_name(a.category)),
        )
    }
}

/// Type and enum-value differences between two sides, reported under `code`.
fn type_diffs(
    q: &str,
    what: &str,
    code: &'static str,
    e: &TypeRef,
    a: &TypeRef,
    out: &mut Vec<Difference>,
) {
    if !same_type(e, a) {
        let (l, r) = fmt_pair(e, a);
        out.push(diff(
            code,
            Severity::Error,
            format!("{q}: {what} {l} → {r}"),
        ));
    } else if e.enum_values.is_some() != a.enum_values.is_some() {
        let show = |t: &TypeRef| match &t.enum_values {
            Some(v) => format!("[{}]", v.join(", ")),
            None => "none".to_string(),
        };
        out.push(diff(
            code,
            Severity::Error,
            format!(
                "{q}: {what} {} enum values {} → {}",
                fmt_type(a),
                show(e),
                show(a)
            ),
        ));
    } else {
        enum_diffs(q, what, e, a, out);
    }
}

fn diff(code: &'static str, severity: Severity, message: String) -> Difference {
    Difference {
        code,
        severity,
        message,
    }
}

/// Enum value differences for two types of the same identity.
fn enum_diffs(q: &str, what: &str, e: &TypeRef, a: &TypeRef, out: &mut Vec<Difference>) {
    let (Some(ev), Some(av)) = (&e.enum_values, &a.enum_values) else {
        return;
    };
    let lost: Vec<&str> = ev
        .iter()
        .filter(|v| !av.contains(v))
        .map(String::as_str)
        .collect();
    let gained: Vec<&str> = av
        .iter()
        .filter(|v| !ev.contains(v))
        .map(String::as_str)
        .collect();
    if !lost.is_empty() {
        out.push(diff(
            "QF047",
            Severity::Error,
            format!(
                "{q}: {what} {} lost values: {}",
                fmt_type(a),
                lost.join(", ")
            ),
        ));
    }
    if !gained.is_empty() {
        out.push(diff(
            "QF047",
            Severity::Warning,
            format!(
                "{q}: {what} {} gained values: {}",
                fmt_type(a),
                gained.join(", ")
            ),
        ));
    }
}

/// The keys of a pick or pick-spread parameter (none for scalars and spreads).
fn pick_keys(p: &Param) -> &[PickKeyMeta] {
    match &p.transform {
        Transform::Pick { keys } | Transform::PickSpread { keys } => keys,
        Transform::Scalar | Transform::Spread => &[],
    }
}

/// Pick keys matched by name: each one's type, then missing keys, then added ones (QF045).
fn key_diffs(
    q: &str,
    param: &str,
    expected: &[PickKeyMeta],
    actual: &[PickKeyMeta],
    out: &mut Vec<Difference>,
) {
    let mut removed = Vec::new();
    for e in expected {
        let what = format!("parameter :{param}.{}", e.name);
        match actual.iter().find(|k| k.name == e.name) {
            Some(a) => type_diffs(q, &what, "QF045", &e.db_type, &a.db_type, out),
            None => removed.push(diff(
                "QF045",
                Severity::Error,
                format!("{q}: {what} removed"),
            )),
        }
    }
    out.extend(removed);
    for a in actual {
        if !expected.iter().any(|k| k.name == a.name) {
            out.push(diff(
                "QF045",
                Severity::Error,
                format!(
                    "{q}: parameter :{param}.{} {} added",
                    a.name,
                    fmt_type(&a.db_type)
                ),
            ));
        }
    }
}

/// Compare the committed `expected` meta with parameters/columns reported by the target.
/// Output order: expected items in order, then added ones in actual order.
pub fn compare(
    expected: &QueryMeta,
    actual_params: &[Param],
    actual_columns: &[Column],
) -> Vec<Difference> {
    let q = expected.name.as_str();
    let mut out = Vec::new();

    for e in &expected.params {
        let what = format!("parameter :{}", e.name);
        match actual_params.iter().find(|p| p.name == e.name) {
            None => out.push(diff(
                "QF045",
                Severity::Error,
                format!("{q}: {what} removed"),
            )),
            Some(a) => {
                type_diffs(q, &what, "QF045", &e.db_type, &a.db_type, &mut out);
                key_diffs(q, &e.name, pick_keys(e), pick_keys(a), &mut out);
            }
        }
    }
    for a in actual_params {
        if !expected.params.iter().any(|p| p.name == a.name) {
            out.push(diff(
                "QF045",
                Severity::Error,
                format!("{q}: parameter :{} {} added", a.name, fmt_type(&a.db_type)),
            ));
        }
    }

    for e in &expected.columns {
        let what = format!("column \"{}\"", e.name);
        let Some(a) = actual_columns.iter().find(|c| c.name == e.name) else {
            out.push(diff(
                "QF040",
                Severity::Error,
                format!("{q}: {what} missing"),
            ));
            continue;
        };
        type_diffs(q, &what, "QF041", &e.db_type, &a.db_type, &mut out);
        if !e.nullable && a.nullable {
            out.push(diff(
                "QF042",
                Severity::Error,
                format!("{q}: {what} became nullable"),
            ));
        } else if e.nullable && !a.nullable {
            out.push(diff(
                "QF044",
                Severity::Note,
                format!("{q}: {what} became non-null"),
            ));
        }
    }
    for a in actual_columns {
        if !expected.columns.iter().any(|c| c.name == a.name) {
            out.push(diff(
                "QF043",
                Severity::Note,
                format!("{q}: column \"{}\" {} added", a.name, fmt_type(&a.db_type)),
            ));
        }
    }
    out
}
