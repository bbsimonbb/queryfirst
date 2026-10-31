//! Semantic diff between a committed meta and what a target database reports.

use crate::model::{Column, Param, QueryMeta, Severity, TypeRef};

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
    a.schema == b.schema && a.name == b.name && a.modifier == b.modifier && a.array == b.array
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
            Some(a) if !same_type(&e.db_type, &a.db_type) => out.push(diff(
                "QF045",
                Severity::Error,
                format!(
                    "{q}: {what} {} → {}",
                    fmt_type(&e.db_type),
                    fmt_type(&a.db_type)
                ),
            )),
            Some(a) => enum_diffs(q, &what, &e.db_type, &a.db_type, &mut out),
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
        if same_type(&e.db_type, &a.db_type) {
            enum_diffs(q, &what, &e.db_type, &a.db_type, &mut out);
        } else {
            out.push(diff(
                "QF041",
                Severity::Error,
                format!(
                    "{q}: {what} {} → {}",
                    fmt_type(&e.db_type),
                    fmt_type(&a.db_type)
                ),
            ));
        }
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
