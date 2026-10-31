#[test]
fn parses_select() {
    assert!(pg_query::parse("SELECT 1", 0).is_ok());
}
