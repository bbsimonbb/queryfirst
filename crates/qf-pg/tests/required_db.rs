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

/// Row hooks read from the real catalog: own and descendants' BEFORE/INSTEAD OF row triggers and
/// rules flag a table for their operation; disabled, AFTER and statement-level triggers do not.
#[tokio::test]
async fn row_hooks_from_the_catalog() {
    with_db(|db| async move {
        let s = &db.schema;
        db.admin
            .batch_execute(&format!(
                "
CREATE FUNCTION {s}.keep() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$;
CREATE TABLE {s}.plain (id int NOT NULL, v text NOT NULL);
CREATE TABLE {s}.after_only (v text NOT NULL);
CREATE TRIGGER a AFTER INSERT OR UPDATE ON {s}.after_only
    FOR EACH ROW EXECUTE FUNCTION {s}.keep();
CREATE TRIGGER st BEFORE INSERT OR UPDATE ON {s}.after_only
    FOR EACH STATEMENT EXECUTE FUNCTION {s}.keep();
CREATE TABLE {s}.disabled (v text NOT NULL);
CREATE TRIGGER d BEFORE INSERT ON {s}.disabled FOR EACH ROW EXECUTE FUNCTION {s}.keep();
ALTER TABLE {s}.disabled DISABLE TRIGGER d;
CREATE TABLE {s}.events (k int NOT NULL, v text NOT NULL) PARTITION BY RANGE (k);
CREATE TABLE {s}.events_1 PARTITION OF {s}.events FOR VALUES FROM (0) TO (100);
CREATE TRIGGER b BEFORE INSERT ON {s}.events_1 FOR EACH ROW EXECUTE FUNCTION {s}.keep();
CREATE TABLE {s}.base (v text NOT NULL);
CREATE TABLE {s}.child () INHERITS ({s}.base);
CREATE TRIGGER u BEFORE UPDATE ON {s}.child FOR EACH ROW EXECUTE FUNCTION {s}.keep();
CREATE TABLE {s}.redirected (v text NOT NULL);
CREATE RULE r AS ON INSERT TO {s}.redirected DO INSTEAD NOTHING;
"
            ))
            .await
            .expect("create hooks");
        let cat = qf_pg::Catalog::load(&db.pool)
            .await
            .expect("reload catalog");
        let flags = |t: &str| {
            let t = cat
                .table(Some(s), t)
                .unwrap_or_else(|| panic!("no table {t}"));
            (t.hooks_insert, t.hooks_update)
        };
        assert_eq!(flags("plain"), (false, false));
        assert_eq!(flags("after_only"), (false, false));
        assert_eq!(flags("disabled"), (false, false));
        assert_eq!(flags("events_1"), (true, false));
        assert_eq!(
            flags("events"),
            (true, false),
            "a partition's trigger flags the parent"
        );
        assert_eq!(flags("child"), (false, true));
        assert_eq!(
            flags("base"),
            (false, true),
            "a child's trigger flags the parent"
        );
        assert_eq!(flags("redirected"), (true, false));
        assert_eq!(flags("posts"), (true, false));
        assert_eq!(flags("drafts"), (false, true));

        let order = vec!["v".to_string()];
        let required = |sql: &str| infer_required(sql, &order, &[], &cat)[0].required;
        assert!(required(&format!(
            "INSERT INTO {s}.plain (id, v) VALUES (1, $1)"
        )));
        assert!(required(&format!(
            "INSERT INTO {s}.after_only (v) VALUES ($1)"
        )));
        assert!(!required(&format!(
            "INSERT INTO {s}.events (k, v) VALUES (1, $1)"
        )));
        assert!(required(&format!("INSERT INTO {s}.base (v) VALUES ($1)")));
        assert!(!required(&format!("UPDATE {s}.base SET v = $1")));
        assert!(!required(&format!(
            "INSERT INTO {s}.redirected (v) VALUES ($1)"
        )));
    })
    .await;
}
