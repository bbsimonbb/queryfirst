//! Pure nullability tests: every fixture in `tests/nullability/` against an in-memory catalog.
//!
//! Without a database there is no describe, so the `Described` input is built by hand from the
//! fixture's `-- expect:` line: one column per expected name, in order, with no `source`. The
//! analyzer itself derives everything from the AST and the catalog; `nullability_db.rs` runs
//! the same fixtures through a real describe (where sources are filled in).

mod support;

use qf_meta::NullableReason;
use qf_pg::{
    ColumnNullability, Described, DescribedColumn, analyze_columns, apply_alias_overrides,
};
use support::{FakeCatalog, dummy_type, fixtures, show, stripped};

fn described(names: &[&str]) -> Described {
    Described {
        params: Vec::new(),
        columns: names
            .iter()
            .map(|n| DescribedColumn {
                name: n.to_string(),
                db_type: dummy_type(),
                source: None,
            })
            .collect(),
    }
}

const GAVE_UP: ColumnNullability = ColumnNullability {
    nullable: true,
    reason: NullableReason::AnalyzerGaveUp,
};
const PROVEN: ColumnNullability = ColumnNullability {
    nullable: false,
    reason: NullableReason::ProvenNotnull,
};

#[test]
fn nullability_fixtures() {
    let cat = FakeCatalog::fixture_schema();
    let mut failures = Vec::new();
    for f in fixtures("nullability") {
        let raw: Vec<&str> = f.expect.iter().map(|(n, _)| n.as_str()).collect();
        let mut cols = analyze_columns(&f.sql, &described(&raw), &cat);
        let mut names: Vec<String> = raw.iter().map(|s| s.to_string()).collect();
        apply_alias_overrides(&mut cols, &mut names);
        assert_eq!(cols.len(), f.expect.len(), "{}: column count", f.name);
        for (i, (raw_name, want)) in f.expect.iter().enumerate() {
            assert_eq!(names[i], stripped(raw_name), "{}: name {i}", f.name);
            if cols[i] != *want {
                failures.push(format!(
                    "{}: {raw_name}: want {}, got {}",
                    f.name,
                    show(want),
                    show(&cols[i])
                ));
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn nullability_column_count_mismatch_gives_up_on_all() {
    let cat = FakeCatalog::fixture_schema();
    let cols = analyze_columns("SELECT id, email FROM customers", &described(&["id"]), &cat);
    assert_eq!(cols, [GAVE_UP]);
}

#[test]
fn nullability_name_mismatch_gives_up_on_all() {
    let cat = FakeCatalog::fixture_schema();
    let cols = analyze_columns(
        "SELECT id, email FROM customers",
        &described(&["id", "name"]),
        &cat,
    );
    assert_eq!(cols, [GAVE_UP, GAVE_UP]);
}

#[test]
fn nullability_unparsable_or_multiple_statements_give_up() {
    let cat = FakeCatalog::fixture_schema();
    let d = described(&["id"]);
    assert_eq!(analyze_columns("SELEC id", &d, &cat), [GAVE_UP]);
    assert_eq!(
        analyze_columns("SELECT id FROM customers; SELECT 1", &d, &cat),
        [GAVE_UP]
    );
    assert_eq!(
        analyze_columns("SELECT 1 AS id FROM no_such_table", &d, &cat),
        [PROVEN]
    );
    assert_eq!(
        analyze_columns("SELECT id FROM no_such_table", &d, &cat),
        [GAVE_UP]
    );
}

#[test]
fn nullability_source_that_is_nullable_vetoes_proof() {
    // Should the AST walk ever disagree with what Postgres says the column comes from, the
    // catalog entry for the source wins when it says nullable.
    let cat = FakeCatalog::fixture_schema();
    let mut d = described(&["id"]);
    d.columns[0].source = cat.source("customers", "name");
    assert_eq!(
        analyze_columns("SELECT id FROM customers", &d, &cat),
        [GAVE_UP]
    );
    d.columns[0].source = cat.source("customers", "id");
    assert_eq!(
        analyze_columns("SELECT id FROM customers", &d, &cat),
        [PROVEN]
    );
}

#[test]
fn nullability_returning_old_new_gives_up() {
    // PG 18: OLD in INSERT … RETURNING and NEW in DELETE … RETURNING are all-null rows.
    let cat = FakeCatalog::fixture_schema();
    let d = described(&["id"]);
    for sql in [
        "INSERT INTO customers (email) VALUES ($1) RETURNING old.id",
        "DELETE FROM customers WHERE id = $1 RETURNING new.id",
        "UPDATE customers SET name = $1 RETURNING WITH (OLD AS o) o.id",
        "INSERT INTO customers (email) VALUES ($1) RETURNING WITH (NEW AS n) id",
    ] {
        assert_eq!(analyze_columns(sql, &d, &cat), [GAVE_UP], "{sql}");
    }
    assert_eq!(
        analyze_columns(
            "INSERT INTO customers (email) VALUES ($1) RETURNING id",
            &d,
            &cat
        ),
        [PROVEN]
    );
}

#[test]
fn nullability_schema_qualified_refs() {
    let cat = FakeCatalog::fixture_schema();
    let d = described(&["email", "line1"]);
    let cols = analyze_columns(
        "SELECT public.customers.email, a.line1 FROM public.customers \
         LEFT JOIN public.addresses a ON a.customer_id = customers.id",
        &d,
        &cat,
    );
    assert_eq!(
        cols,
        [
            PROVEN,
            ColumnNullability {
                nullable: true,
                reason: NullableReason::OuterJoin
            }
        ]
    );
}

#[test]
fn nullability_alias_overrides() {
    let mut cols = vec![GAVE_UP, PROVEN, GAVE_UP, PROVEN];
    let mut names: Vec<String> = ["total!", "name?", "plain", "!"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    apply_alias_overrides(&mut cols, &mut names);
    assert_eq!(names, ["total", "name", "plain", "!"]);
    let over = |nullable| ColumnNullability {
        nullable,
        reason: NullableReason::Override,
    };
    assert_eq!(cols, [over(false), over(true), GAVE_UP, PROVEN]);
}
