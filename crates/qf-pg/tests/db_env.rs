//! Tests behind `db-tests` require a database and fail (never skip) without one.
#![cfg(feature = "db-tests")]

#[test]
fn test_database_url_is_set() {
    let url = std::env::var("QF_TEST_DATABASE_URL")
        .expect("QF_TEST_DATABASE_URL must be set when running with --features db-tests");
    assert!(!url.is_empty());
}
