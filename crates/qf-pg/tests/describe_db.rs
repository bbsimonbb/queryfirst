//! Describe, catalog and error enrichment against a real Postgres.
//!
//! Needs `QF_TEST_DATABASE_URL`; fails (never skips) without one. Each test loads
//! `tests/fixtures/schema.sql` into its own uniquely named schema (selected through
//! `search_path`) and drops it afterwards, so any Postgres will do.
#![cfg(feature = "db-tests")]

use qf_meta::TypeCategory;
use qf_pg::{Catalog, ConnSource, Pool, connect_pool, describe, resolve_connection, to_diagnostic};
use qf_sqlfile::{LexedQuery, Severity, lex_params, normalise, parse_query_file};
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

const FIXTURE: &str = include_str!("fixtures/schema.sql");

struct Db {
    pool: Pool,
    catalog: Arc<Catalog>,
    admin: Arc<tokio_postgres::Client>,
    schema: String,
}

fn database_url() -> String {
    std::env::var("QF_TEST_DATABASE_URL")
        .expect("QF_TEST_DATABASE_URL must be set when running with --features db-tests")
}

fn unique_schema() -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    format!(
        "qf_test_{}_{}_{}",
        std::process::id(),
        nanos,
        N.fetch_add(1, Ordering::Relaxed)
    )
}

fn with_search_path(url: &str, schema: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}options=-c%20search_path%3D{schema}")
}

/// Create a temp schema with the fixture, run `body`, then drop the schema even if `body` panics.
async fn with_db<F, Fut>(body: F)
where
    F: FnOnce(Db) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let url = database_url();
    let schema = unique_schema();
    let (admin, conn) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_postgres::connect(&url, tokio_postgres::NoTls),
    )
    .await
    .expect("timed out connecting to QF_TEST_DATABASE_URL")
    .expect("cannot connect to QF_TEST_DATABASE_URL");
    tokio::spawn(conn);
    admin
        .batch_execute(&format!(
            "CREATE SCHEMA {schema}; SET search_path TO {schema};\n{FIXTURE}"
        ))
        .await
        .expect("load fixture schema");
    let admin = Arc::new(admin);

    let resolved =
        resolve_connection(&[ConnSource::Explicit(with_search_path(&url, &schema))]).unwrap();
    let outcome = async {
        let pool = connect_pool(&resolved, 2, Duration::from_secs(10))
            .await
            .expect("connect_pool");
        let catalog = Arc::new(Catalog::load(&pool).await.expect("Catalog::load"));
        let db = Db {
            pool,
            catalog,
            admin: admin.clone(),
            schema: schema.clone(),
        };
        tokio::spawn(body(db)).await
    }
    .await;

    admin
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .expect("drop temp schema");
    if let Err(e) = outcome {
        std::panic::resume_unwind(e.into_panic());
    }
}

fn lex(src: &str) -> LexedQuery {
    let q = parse_query_file(Path::new("q.sql"), &normalise(src)).expect("parse query file");
    lex_params(&q).expect("lex params")
}

#[tokio::test]
async fn describes_params_and_columns() {
    with_db(|db| async move {
        let lexed = lex(concat!(
            "SELECT c.id, c.email, i.total, c.tags, c.prefs, c.created_at, c.birthday, c.access,\n",
            "       1 + 1 AS two\n",
            "FROM customers c JOIN invoices i ON i.customer_id = c.id\n",
            "WHERE c.id = :id AND i.total > :minTotal AND c.email = :email"
        ));
        let d = describe(&db.pool, &lexed.db_statement)
            .await
            .expect("describe");

        let p: Vec<(&str, &str, bool)> = d
            .params
            .iter()
            .map(|t| (t.schema.as_str(), t.name.as_str(), t.array))
            .collect();
        assert_eq!(
            p,
            [
                ("pg_catalog", "int4", false),
                ("pg_catalog", "numeric", false),
                // There is no varchar = varchar operator: Postgres compares as text.
                ("pg_catalog", "text", false)
            ]
        );
        assert!(d.params.iter().all(|t| t.modifier.is_none()));

        // An assignment target types the parameter as the column's type.
        let upd = lex("UPDATE customers SET email = :email WHERE id = :id");
        let u = describe(&db.pool, &upd.db_statement)
            .await
            .expect("describe update");
        let p: Vec<&str> = u.params.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(p, ["varchar", "int4"]);

        let names: Vec<&str> = d.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "id",
                "email",
                "total",
                "tags",
                "prefs",
                "created_at",
                "birthday",
                "access",
                "two"
            ]
        );
        let col = |n: &str| d.columns.iter().find(|c| c.name == n).unwrap();
        let ty = |n: &str| {
            let t = &col(n).db_type;
            (t.name.as_str(), t.array, t.modifier.as_deref(), t.category)
        };
        assert_eq!(ty("id"), ("int4", false, None, TypeCategory::Base));
        assert_eq!(
            ty("email"),
            ("varchar", false, Some("(30)"), TypeCategory::Base)
        );
        assert_eq!(
            ty("total"),
            ("numeric", false, Some("(12,2)"), TypeCategory::Base)
        );
        assert_eq!(ty("tags"), ("text", true, None, TypeCategory::Base));
        assert_eq!(ty("prefs"), ("jsonb", false, None, TypeCategory::Base));
        assert_eq!(
            ty("created_at"),
            ("timestamp", false, None, TypeCategory::Base)
        );
        assert_eq!(ty("birthday"), ("date", false, None, TypeCategory::Base));
        assert_eq!(
            ty("access"),
            ("accessLevel", false, None, TypeCategory::Enum)
        );
        assert_eq!(col("access").db_type.schema, db.schema);
        assert_eq!(ty("two"), ("int4", false, None, TypeCategory::Base));
        assert_eq!(col("two").source, None);

        let customers = db
            .catalog
            .table(None, "customers")
            .expect("customers table");
        let invoices = db
            .catalog
            .table(Some(&db.schema), "invoices")
            .expect("invoices table");
        assert_eq!(customers.schema, db.schema);
        assert_eq!(col("id").source, Some((customers.oid, 1)));
        assert_eq!(col("email").source, Some((customers.oid, 2)));
        assert_eq!(col("total").source.map(|s| s.0), Some(invoices.oid));

        let (oid, attnum) = col("email").source.unwrap();
        let email = db.catalog.column(oid, attnum).expect("email column");
        assert_eq!(
            (
                email.schema.as_str(),
                email.table.as_str(),
                email.name.as_str()
            ),
            (db.schema.as_str(), "customers", "email")
        );
        assert!(email.not_null && !email.has_default);
        assert_eq!(email.db_type.modifier.as_deref(), Some("(30)"));

        let id = db.catalog.column(customers.oid, 1).unwrap();
        assert!(id.not_null && id.has_default);
        let name = db.catalog.column(customers.oid, 3).unwrap();
        assert!(!name.not_null && !name.has_default);
        let access = db.catalog.column(customers.oid, 4).unwrap();
        assert!(access.not_null && access.has_default);
        let (toid, tnum) = col("total").source.unwrap();
        let total = db.catalog.column(toid, tnum).unwrap();
        assert!(total.not_null && total.has_default);
        assert_eq!(total.db_type.modifier.as_deref(), Some("(12,2)"));
        let tags = db.catalog.column(customers.oid, 5).unwrap();
        assert_eq!(
            (tags.db_type.name.as_str(), tags.db_type.array),
            ("text", true)
        );

        assert!(!db.catalog.timezone().is_empty());
        assert!(db.catalog.table(None, "no_such_table").is_none());
    })
    .await;
}

#[tokio::test]
async fn enum_type_ref_has_sorted_values() {
    with_db(|db| async move {
        let sorted = Some(vec!["admin".to_string(), "owner".into(), "user".into()]);
        let d = describe(&db.pool, "SELECT access FROM customers WHERE access = $1")
            .await
            .unwrap();
        assert_eq!(d.columns[0].db_type.enum_values, sorted);
        assert_eq!(d.params[0].enum_values, sorted);
        assert_eq!(d.params[0].category, TypeCategory::Enum);

        let customers = db.catalog.table(None, "customers").unwrap();
        let access = db.catalog.column(customers.oid, 4).unwrap();
        assert_eq!(access.db_type.name, "accessLevel");
        assert_eq!(access.db_type.category, TypeCategory::Enum);
        assert_eq!(access.db_type.enum_values, sorted);

        // Non-enum types carry no values.
        assert_eq!(d.columns.len(), 1);
        let email = db.catalog.column(customers.oid, 2).unwrap();
        assert_eq!(email.db_type.enum_values, None);
    })
    .await;
}

#[tokio::test]
async fn undetermined_param_type_is_qf011_with_cast_hint() {
    with_db(|db| async move {
        // (Not `SELECT :p`: Postgres types a bare select-list parameter as text.)
        // Never typed at all: 42P18, without a position. Typed by one use but not by another:
        // 42P08, positioned at the untyped use.
        let cases = [
            (
                "SELECT 1 WHERE :p IS NULL",
                "42P18",
                "could not determine the type of parameter :p",
                16,
            ),
            (
                "SELECT id FROM customers WHERE :p IS NULL OR email = :p",
                "42P08",
                "could not infer a single type for parameter :p",
                32,
            ),
        ];
        for (sql, sqlstate, message, col) in cases {
            let lexed = lex(sql);
            let err = describe(&db.pool, &lexed.db_statement)
                .await
                .expect_err("Postgres cannot type the parameter");
            assert!(!qf_pg::is_connection_error(&err));
            match &err {
                qf_pg::PgQueryError::Db { code, .. } => assert_eq!(code, sqlstate, "{sql}"),
                other => panic!("{other:?}"),
            }
            let diag = to_diagnostic(&err, &lexed, &db.catalog);
            assert_eq!(diag.code, "QF011", "{sql}");
            assert_eq!(diag.severity, Severity::Error);
            assert_eq!(diag.message, message, "{sql}");
            let hint = diag.hint.expect("hint");
            assert!(hint.contains(":p::"), "{hint}");
            assert_eq!((diag.line, diag.col), (1, col), "{sql}");
        }
    })
    .await;
}

/// 42P18 is not always about a parameter: `ARRAY[]` keeps the server's message and hint (the
/// `ARRAY[]::integer[]` cast), as QF010 at `ARRAY`.
#[tokio::test]
async fn empty_array_is_qf010_with_the_server_hint() {
    with_db(|db| async move {
        let lexed = lex("SELECT ARRAY[] AS a, :p::int AS p");
        let err = describe(&db.pool, &lexed.db_statement).await.unwrap_err();
        let qf_pg::PgQueryError::Db {
            code,
            message,
            hint,
            position,
            ..
        } = &err
        else {
            panic!("{err:?}");
        };
        assert_eq!((code.as_str(), *position), ("42P18", Some(8)));
        let diag = to_diagnostic(&err, &lexed, &db.catalog);
        assert_eq!(diag.code, "QF010");
        assert_eq!(&diag.message, message);
        assert_eq!(&diag.hint, hint);
        assert!(
            diag.hint.as_deref().unwrap_or("").contains("ARRAY[]::"),
            "{hint:?}"
        );
        assert_eq!((diag.line, diag.col), (1, 8));
    })
    .await;
}

#[tokio::test]
async fn undefined_column_did_you_mean() {
    with_db(|db| async move {
        let lexed = lex("SELECT id, emial FROM customers");
        let err = describe(&db.pool, &lexed.db_statement).await.unwrap_err();
        let diag = to_diagnostic(&err, &lexed, &db.catalog);
        assert_eq!(diag.code, "QF010");
        assert_eq!(diag.severity, Severity::Error);
        assert_eq!(
            diag.message,
            r#"column "emial" does not exist on "customers"; did you mean "email"?"#
        );
        assert_eq!((diag.line, diag.col), (1, 12));

        let lexed = lex("SELECT id FROM custmers");
        let err = describe(&db.pool, &lexed.db_statement).await.unwrap_err();
        let diag = to_diagnostic(&err, &lexed, &db.catalog);
        assert_eq!(diag.code, "QF010");
        assert_eq!(
            diag.message,
            r#"relation "custmers" does not exist; did you mean "customers"?"#
        );

        // Whatever the server's `lc_messages`, the name comes from the statement at the error
        // position. (sql, message, col)
        let s = &db.schema;
        let on_customers =
            r#"column "emial" does not exist on "customers"; did you mean "email"?"#.to_string();
        let cases = [
            (
                "SELECT c.emial FROM customers c".to_string(),
                on_customers.clone(),
                8,
            ),
            (
                "UPDATE customers SET emial = 'x'".to_string(),
                on_customers.clone(),
                22,
            ),
            (
                "INSERT INTO customers (emial) VALUES ('x')".to_string(),
                on_customers,
                24,
            ),
            (
                "SELECT i.totl FROM invoices i".to_string(),
                r#"column "totl" does not exist on "invoices"; did you mean "total"?"#.to_string(),
                8,
            ),
            (
                format!("SELECT 1 FROM {s}.invoics"),
                format!(r#"relation "{s}.invoics" does not exist; did you mean "invoices"?"#),
                15,
            ),
        ];
        for (sql, message, col) in cases {
            let lexed = lex(&sql);
            let err = describe(&db.pool, &lexed.db_statement).await.unwrap_err();
            let diag = to_diagnostic(&err, &lexed, &db.catalog);
            assert_eq!(diag.code, "QF010", "{sql}");
            assert_eq!(diag.message, message, "{sql}");
            assert_eq!((diag.line, diag.col), (1, col), "{sql}");
        }

        // 42P01 "missing FROM-clause entry" is not a missing relation: the server's message.
        let lexed = lex("SELECT x.id FROM customers");
        let err = describe(&db.pool, &lexed.db_statement).await.unwrap_err();
        let diag = to_diagnostic(&err, &lexed, &db.catalog);
        let qf_pg::PgQueryError::Db { code, message, .. } = &err else {
            panic!("{err:?}");
        };
        assert_eq!(code, "42P01");
        assert_eq!(&diag.message, message);
    })
    .await;
}

#[tokio::test]
async fn error_position_maps_to_source_line_col_with_accents() {
    with_db(|db| async move {
        let src = concat!(
            "/* @name accents */\n",
            "SELECT 'éèà' AS s, :label::text AS l,\n",
            "       'ça' AS t, nonexistent\n",
            "FROM customers"
        );
        let lexed = lex(src);
        let err = describe(&db.pool, &lexed.db_statement).await.unwrap_err();
        let diag = to_diagnostic(&err, &lexed, &db.catalog);
        assert_eq!(diag.code, "QF010");
        let line = src.lines().position(|l| l.contains("nonexistent")).unwrap() + 1;
        let text = src.lines().nth(line - 1).unwrap();
        let col = text[..text.find("nonexistent").unwrap()].chars().count() + 1;
        assert_eq!((diag.line, diag.col), (line as u32, col as u32));
        assert_eq!((diag.line, diag.col), (3, 19));
    })
    .await;
}

#[tokio::test]
async fn describe_never_executes() {
    with_db(|db| async move {
        let count = |admin: Arc<tokio_postgres::Client>, schema: String| async move {
            let row = admin
                .query_one(&format!("SELECT count(*) FROM {schema}.customers"), &[])
                .await
                .unwrap();
            row.get::<_, i64>(0)
        };
        let before = count(db.admin.clone(), db.schema.clone()).await;
        assert_eq!(before, 2);
        let d = describe(&db.pool, "DELETE FROM customers").await.unwrap();
        assert!(d.params.is_empty() && d.columns.is_empty());
        let d = describe(
            &db.pool,
            "DELETE FROM customers WHERE id = $1 RETURNING email",
        )
        .await
        .unwrap();
        assert_eq!(d.columns[0].name, "email");
        assert_eq!(count(db.admin.clone(), db.schema.clone()).await, before);
    })
    .await;
}
