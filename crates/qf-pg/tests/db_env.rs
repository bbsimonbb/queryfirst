//! Tests behind `db-tests` require a database and fail (never skip) without one.
#![cfg(feature = "db-tests")]

#[test]
fn test_database_url_is_set() {
    let url = std::env::var("QF_TEST_DATABASE_URL")
        .expect("QF_TEST_DATABASE_URL must be set when running with --features db-tests");
    assert!(!url.is_empty());
}

/// Sessions opened through `ResolvedConn::read_only` refuse writes.
#[tokio::test]
async fn read_only_sessions_refuse_writes() {
    use qf_pg::{ConnSource, connect_pool, resolve_connection};
    let url = std::env::var("QF_TEST_DATABASE_URL").unwrap();
    let conn = resolve_connection(&[ConnSource::Explicit(url)])
        .unwrap()
        .read_only();
    let pool = connect_pool(&conn, 2, std::time::Duration::from_secs(10))
        .await
        .unwrap();
    // Both clients at once, so both sessions are checked.
    let (a, b) = (pool.get().await, pool.get().await);
    for c in [&a, &b] {
        let row = c
            .query_one("SHOW default_transaction_read_only", &[])
            .await
            .unwrap();
        assert_eq!(row.get::<_, String>(0), "on");
        let err = c
            .batch_execute("CREATE TEMP TABLE qf_read_only_probe (x int)")
            .await
            .unwrap_err();
        assert_eq!(err.code().map(|c| c.code()), Some("25006"), "{err}");
    }
}
