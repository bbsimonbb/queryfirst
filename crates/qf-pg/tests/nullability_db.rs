//! Every nullability fixture against a real Postgres: the statement is described (so result
//! names, order and `source` come from the server), then analyzed with the loaded `Catalog`.
//!
//! Needs `QF_TEST_DATABASE_URL`; fails (never skips) without one.
#![cfg(feature = "db-tests")]

mod support;

use qf_pg::{analyze_columns, apply_alias_overrides, describe};
use support::db::with_db;
use support::{fixtures, show, stripped};

#[tokio::test]
async fn nullability_fixtures_against_postgres() {
    with_db(|db| async move {
        let mut failures = Vec::new();
        for f in fixtures("nullability") {
            let d = match describe(&db.pool, &f.sql).await {
                Ok(d) => d,
                Err(e) => {
                    failures.push(format!("{}: describe failed: {e:?}", f.name));
                    continue;
                }
            };
            let described: Vec<&str> = d.columns.iter().map(|c| c.name.as_str()).collect();
            let expected: Vec<&str> = f.expect.iter().map(|(n, _)| n.as_str()).collect();
            if described != expected {
                failures.push(format!(
                    "{}: Postgres reports columns {described:?}, fixture expects {expected:?}",
                    f.name
                ));
                continue;
            }
            let mut cols = analyze_columns(&f.sql, &d, db.catalog.as_ref());
            let mut names: Vec<String> = d.columns.iter().map(|c| c.name.clone()).collect();
            apply_alias_overrides(&mut cols, &mut names);
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
    })
    .await;
}

/// PG 18 keeps `attnotnull` for a `NOT NULL … NOT VALID` constraint, yet existing rows may hold
/// nulls: the catalog must not report such a column as non-null.
#[tokio::test]
async fn not_valid_not_null_is_not_trusted() {
    with_db(|db| async move {
        let version: i32 = db
            .admin
            .query_one("SELECT current_setting('server_version_num')::int", &[])
            .await
            .unwrap()
            .get(0);
        if version < 180000 {
            return; // NOT VALID not-null constraints do not exist before PG 18.
        }
        let s = &db.schema;
        db.admin
            .batch_execute(&format!(
                "CREATE TABLE {s}.legacy (a int, b int);
                 INSERT INTO {s}.legacy VALUES (NULL, 1);
                 ALTER TABLE {s}.legacy ADD CONSTRAINT legacy_a_nn NOT NULL a NOT VALID;
                 ALTER TABLE {s}.legacy ALTER COLUMN b SET NOT NULL;"
            ))
            .await
            .unwrap();
        let catalog = qf_pg::Catalog::load(&db.pool).await.unwrap();
        let t = catalog.table(Some(s), "legacy").expect("legacy table");
        assert!(
            !t.columns[0].not_null,
            "NOT VALID must not count as NOT NULL"
        );
        assert!(t.columns[1].not_null);
    })
    .await;
}
