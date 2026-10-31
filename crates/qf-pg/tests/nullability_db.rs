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

async fn server_version(db: &support::db::Db) -> i32 {
    db.admin
        .query_one("SELECT current_setting('server_version_num')::int", &[])
        .await
        .unwrap()
        .get(0)
}

/// `not_null` of `table.column` in a freshly loaded catalog.
async fn not_null(db: &support::db::Db, table: &str, column: &str) -> bool {
    let catalog = qf_pg::Catalog::load(&db.pool).await.unwrap();
    let t = catalog
        .table(Some(&db.schema), table)
        .unwrap_or_else(|| panic!("table {table}"));
    t.columns
        .iter()
        .find(|c| c.name == column)
        .unwrap_or_else(|| panic!("column {table}.{column}"))
        .not_null
}

/// A parent's NOT NULL is trusted when every descendant enforces it too.
#[tokio::test]
async fn inherited_not_null_is_trusted_when_descendants_agree() {
    with_db(|db| async move {
        let s = &db.schema;
        db.admin
            .batch_execute(&format!(
                "CREATE TABLE {s}.p (a int NOT NULL, b int);
                 CREATE TABLE {s}.ch () INHERITS ({s}.p);
                 CREATE TABLE {s}.gch () INHERITS ({s}.ch);
                 CREATE TABLE {s}.pt (a int NOT NULL, k int NOT NULL) PARTITION BY LIST (k);
                 CREATE TABLE {s}.pt1 PARTITION OF {s}.pt FOR VALUES IN (1);"
            ))
            .await
            .unwrap();
        assert!(not_null(&db, "p", "a").await);
        assert!(!not_null(&db, "p", "b").await);
        assert!(not_null(&db, "ch", "a").await);
        assert!(not_null(&db, "pt", "a").await);
        assert!(not_null(&db, "pt1", "a").await);
    })
    .await;
}

/// Traditional inheritance: where Postgres lets a child drop an inherited NOT NULL, the parent's
/// NOT NULL no longer holds for `SELECT … FROM parent`.
#[tokio::test]
async fn child_dropping_not_null_untrusts_parent() {
    with_db(|db| async move {
        let s = &db.schema;
        db.admin
            .batch_execute(&format!(
                "CREATE TABLE {s}.p (a int NOT NULL);
                 CREATE TABLE {s}.ch () INHERITS ({s}.p);
                 CREATE TABLE {s}.gch () INHERITS ({s}.ch);"
            ))
            .await
            .unwrap();
        // Through a grandchild, to exercise the recursive descent.
        let dropped = db
            .admin
            .batch_execute(&format!("ALTER TABLE {s}.gch ALTER COLUMN a DROP NOT NULL"))
            .await;
        match dropped {
            Ok(()) => {
                assert!(!not_null(&db, "gch", "a").await);
                assert!(
                    !not_null(&db, "ch", "a").await,
                    "child of a nullable grandchild"
                );
                assert!(
                    !not_null(&db, "p", "a").await,
                    "ancestor of a nullable grandchild"
                );
            }
            // Versions that forbid it ("column is marked NOT NULL in parent table"): the
            // parent's NOT NULL keeps holding.
            Err(e) => {
                assert!(e.as_db_error().is_some(), "{e:?}");
                assert!(not_null(&db, "p", "a").await);
            }
        }
    })
    .await;
}

/// PG 18: `NOT NULL NO INHERIT` on a parent leaves children nullable.
#[tokio::test]
async fn no_inherit_not_null_untrusts_parent() {
    with_db(|db| async move {
        if server_version(&db).await < 180000 {
            return; // NO INHERIT not-null constraints do not exist before PG 18.
        }
        let s = &db.schema;
        db.admin
            .batch_execute(&format!(
                "CREATE TABLE {s}.p (a int NOT NULL NO INHERIT);
                 CREATE TABLE {s}.ch () INHERITS ({s}.p);
                 INSERT INTO {s}.ch VALUES (NULL);"
            ))
            .await
            .unwrap();
        assert!(!not_null(&db, "ch", "a").await);
        assert!(!not_null(&db, "p", "a").await);
    })
    .await;
}

/// A foreign-table child or partition: its NOT NULL is not enforced, so neither it nor its
/// ancestors are trusted. The wrapper has no handler (nothing is ever read through it); FDW and
/// server are database-wide, so they are named after the temp schema for `with_db` to drop them
/// (also when the test fails).
#[tokio::test]
async fn foreign_descendant_untrusts_parent() {
    with_db(|db| async move {
        let s = &db.schema;
        db.admin
            .batch_execute(&format!(
                "CREATE FOREIGN DATA WRAPPER {s}_fdw;
                 CREATE SERVER {s}_srv FOREIGN DATA WRAPPER {s}_fdw;
                 CREATE TABLE {s}.p (a int NOT NULL);
                 CREATE FOREIGN TABLE {s}.fch () INHERITS ({s}.p) SERVER {s}_srv;
                 CREATE TABLE {s}.pt (a int NOT NULL, k int NOT NULL) PARTITION BY LIST (k);
                 CREATE FOREIGN TABLE {s}.fpt PARTITION OF {s}.pt FOR VALUES IN (1)
                     SERVER {s}_srv;"
            ))
            .await
            .unwrap();
        let got = (
            not_null(&db, "fch", "a").await,
            not_null(&db, "p", "a").await,
            not_null(&db, "fpt", "a").await,
            not_null(&db, "pt", "a").await,
        );
        assert_eq!(got, (false, false, false, false));
    })
    .await;
}
