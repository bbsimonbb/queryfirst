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

/// `QF_TEST_DATABASE_URL` split into the URL without its password, and the password
/// (percent-decoded). Never printed.
fn url_and_password() -> (String, String) {
    let url = std::env::var("QF_TEST_DATABASE_URL").unwrap();
    let (scheme, rest) = url.split_once("://").expect("a postgres:// URL");
    let (userinfo, target) = rest.split_once('@').expect("a URL with user info");
    let (user, pw) = userinfo.split_once(':').expect("a URL with a password");
    let mut decoded = Vec::new();
    let b = pw.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(Ok(v)) = pw.get(i + 1..i + 3).map(|h| u8::from_str_radix(h, 16))
        {
            decoded.push(v);
            i += 3;
            continue;
        }
        decoded.push(b[i]);
        i += 1;
    }
    (
        format!("{scheme}://{user}@{target}"),
        String::from_utf8(decoded).unwrap(),
    )
}

/// An injected environment of exactly `pairs`, the per-user directory pointing at `home` so the
/// real password file is never read.
fn injected(
    home: &std::path::Path,
    pairs: Vec<(&'static str, String)>,
) -> impl Fn(&str) -> Option<String> + use<> {
    let home = home.display().to_string();
    let mut map: std::collections::HashMap<String, String> =
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    map.insert("APPDATA".into(), home.clone());
    map.insert("HOME".into(), home);
    move |k: &str| map.get(k).cloned()
}

async fn select_one(conn: &qf_pg::ResolvedConn) {
    let pool = qf_pg::connect_pool(conn, 1, std::time::Duration::from_secs(10))
        .await
        .unwrap_or_else(|e| panic!("connect failed: {}", e.message));
    let row = pool.get().await.query_one("SELECT 1", &[]).await.unwrap();
    assert_eq!(row.get::<_, i32>(0), 1);
}

/// A URL without a password connects with the password from PGPASSWORD.
#[tokio::test]
async fn passwordless_url_connects_with_pgpassword() {
    use qf_pg::{ConnSource, resolve_connection_with};
    let (url, pw) = url_and_password();
    let home = tempfile::tempdir().unwrap();
    let env = injected(home.path(), vec![("PGPASSWORD", pw)]);
    let conn = resolve_connection_with(&[ConnSource::Explicit(url)], &env).unwrap();
    select_one(&conn).await;
}

/// A URL without a password connects with the password from the password file.
#[tokio::test]
async fn passwordless_url_connects_with_pgpass_file() {
    use qf_pg::{ConnSource, resolve_connection_with};
    let (url, pw) = url_and_password();
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("pgpass");
    let escaped = pw.replace('\\', "\\\\").replace(':', "\\:");
    std::fs::write(&file, format!("other:1:x:y:wrong\n*:*:*:*:{escaped}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let env = injected(
        home.path(),
        vec![("PGPASSFILE", file.display().to_string())],
    );
    let conn = resolve_connection_with(&[ConnSource::Explicit(url)], &env).unwrap();
    select_one(&conn).await;
}
