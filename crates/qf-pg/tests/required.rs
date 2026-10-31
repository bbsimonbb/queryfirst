//! Pure parameter required inference tests: every fixture in `tests/required/` (format in
//! `support::required_fixtures`) against the in-memory catalog.

mod support;

use qf_meta::RequiredReason;
use qf_pg::{ParamRequirement, infer_required};
use support::{FakeCatalog, check_required, required_fixtures};

const NULLABLE: ParamRequirement = ParamRequirement {
    required: false,
    reason: RequiredReason::DefaultNullable,
};

#[test]
fn required_fixtures_pure() {
    let cat = FakeCatalog::fixture_schema();
    let mut failures = Vec::new();
    for f in required_fixtures() {
        let l = &f.lexed;
        assert!(
            pg_query::parse(&l.db_statement, 0).is_ok(),
            "{}: the statement must parse: {}",
            f.name,
            l.db_statement
        );
        let got = infer_required(&l.db_statement, &l.param_order, &l.uses, &cat);
        check_required(&f, &got, &mut failures);
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn required_unparsable_or_multiple_statements_stay_nullable() {
    let cat = FakeCatalog::fixture_schema();
    let order = vec!["email".to_string()];
    for sql in [
        "SELEC id FROM customers WHERE email = $1",
        "SELECT id FROM customers WHERE email = $1; SELECT 1",
        "SELECT id FROM no_such_table WHERE email = $1",
        "INSERT INTO no_such_table (email) VALUES ($1)",
    ] {
        assert_eq!(infer_required(sql, &order, &[], &cat), [NULLABLE], "{sql}");
    }
}

#[test]
fn required_param_order_longer_than_statement() {
    // A `$n` that never appears in the statement is not required.
    let cat = FakeCatalog::fixture_schema();
    let order = vec!["email".to_string(), "unused".to_string()];
    let got = infer_required(
        "SELECT id FROM customers WHERE email = $1",
        &order,
        &[],
        &cat,
    );
    assert_eq!(
        got,
        [
            ParamRequirement {
                required: true,
                reason: RequiredReason::InferredWhereEqNotnull
            },
            NULLABLE
        ]
    );
}
