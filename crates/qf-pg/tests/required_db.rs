//! Every required fixture against a real Postgres: the statement must prepare (so the fixtures
//! are valid SQL for the schema), then inference runs with the loaded `Catalog`.
//!
//! Needs `QF_TEST_DATABASE_URL`; fails (never skips) without one.
#![cfg(feature = "db-tests")]

mod support;

use qf_pg::{describe, infer_required};
use support::db::with_db;
use support::{check_required, required_fixtures};

/// Fixtures that Postgres rightly rejects (the inference must still keep them nullable), or that
/// name `public`, which the per-test schema is not.
const INVALID_IN_DB: &[&str] = &[
    "ambiguous_column",
    "insert_count_mismatch",
    "update_schema_qualified",
];

#[tokio::test]
async fn required_fixtures_against_postgres() {
    with_db(|db| async move {
        let mut failures = Vec::new();
        for f in required_fixtures() {
            let l = &f.lexed;
            let described = describe(&db.pool, &l.db_statement).await;
            match (described.is_ok(), INVALID_IN_DB.contains(&f.name.as_str())) {
                (true, true) => failures.push(format!("{}: expected Postgres to reject", f.name)),
                (false, false) => {
                    failures.push(format!("{}: describe failed: {described:?}", f.name));
                    continue;
                }
                (false, true) => continue,
                (true, false) => {}
            }
            let got = infer_required(
                &l.db_statement,
                &l.param_order,
                &l.uses,
                db.catalog.as_ref(),
            );
            check_required(&f, &got, &mut failures);
        }
        assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    })
    .await;
}
