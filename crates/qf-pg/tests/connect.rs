//! Connection resolution and connect failures. No database needed.

use qf_pg::{ConnSource, connect_pool, resolve_connection};
use std::time::{Duration, Instant};

#[test]
fn explicit_wins() {
    // SAFETY: the variable name is unique to this test.
    unsafe {
        std::env::set_var(
            "QF_TEST_CONN_EXPLICIT_WINS",
            "postgres://u@other:5432/otherdb",
        )
    };
    let c = resolve_connection(&[
        ConnSource::Explicit("postgres://u@h1:5433/db1".into()),
        ConnSource::Env("QF_TEST_CONN_EXPLICIT_WINS".into()),
    ])
    .unwrap();
    assert_eq!(c.display_target, "h1:5433/db1");
}

#[test]
fn env_var_named() {
    // SAFETY: the variable names are unique to this test.
    unsafe { std::env::set_var("QF_TEST_CONN_ENV_NAMED", "postgres://u@h2:5434/db2") };
    unsafe { std::env::remove_var("QF_TEST_CONN_ENV_NAMED_UNSET") };
    let c = resolve_connection(&[
        ConnSource::Env("QF_TEST_CONN_ENV_NAMED_UNSET".into()),
        ConnSource::Env("QF_TEST_CONN_ENV_NAMED".into()),
        ConnSource::Explicit("postgres://u@never:5432/never".into()),
    ])
    .unwrap();
    assert_eq!(c.display_target, "h2:5434/db2");
}

#[test]
fn standard_env_database_url_then_pg_vars() {
    // This is the only test that touches DATABASE_URL and the PG* variables.
    let pg = [
        "PGHOST",
        "PGPORT",
        "PGUSER",
        "PGDATABASE",
        "PGPASSWORD",
        "PGSSLMODE",
    ];
    // SAFETY: no other test reads or writes these variables.
    unsafe {
        for v in pg {
            std::env::remove_var(v);
        }
        std::env::set_var("DATABASE_URL", "postgres://u@h3:5435/db3");
        std::env::set_var("PGHOST", "pghost");
        std::env::set_var("PGPORT", "6543");
        std::env::set_var("PGUSER", "pguser");
        std::env::set_var("PGDATABASE", "pgdb");
        std::env::set_var("PGPASSWORD", "pgsecret");
    }
    let c = resolve_connection(&[ConnSource::StandardEnv]).unwrap();
    assert_eq!(c.display_target, "h3:5435/db3");
    assert_eq!(c.ssl_mode().as_str(), "prefer");

    // PGSSLMODE applies when the connection string has no sslmode of its own.
    unsafe { std::env::set_var("PGSSLMODE", "require") };
    let c = resolve_connection(&[ConnSource::StandardEnv]).unwrap();
    assert_eq!(c.ssl_mode().as_str(), "require");
    let c = resolve_connection(&[ConnSource::Explicit(
        "postgres://u@h:5432/db?sslmode=disable".into(),
    )])
    .unwrap();
    assert_eq!(c.ssl_mode().as_str(), "disable");
    unsafe { std::env::remove_var("PGSSLMODE") };

    unsafe { std::env::remove_var("DATABASE_URL") };
    let c = resolve_connection(&[ConnSource::StandardEnv]).unwrap();
    assert_eq!(c.display_target, "pghost:6543/pgdb");
    assert!(!format!("{c:?}").contains("pgsecret"));
    assert!(!c.to_string().contains("pgsecret"));

    // Nothing configured: StandardEnv yields nothing, so the next source wins.
    unsafe {
        for v in pg {
            std::env::remove_var(v);
        }
    }
    let c = resolve_connection(&[
        ConnSource::StandardEnv,
        ConnSource::Explicit("postgres://u@h4:5436/db4".into()),
    ])
    .unwrap();
    assert_eq!(c.display_target, "h4:5436/db4");

    // No source at all is a config error.
    let e = resolve_connection(&[ConnSource::StandardEnv]).unwrap_err();
    assert_eq!(e.code, "QF091");
}

fn echo(s: &str) -> String {
    if cfg!(windows) {
        format!("cmd /c echo {s}")
    } else {
        format!("echo {s}")
    }
}

#[test]
fn command_helper_stdout() {
    let c = resolve_connection(&[ConnSource::Command(echo("postgres://u:p@h/db"))]).unwrap();
    assert_eq!(c.display_target, "h:5432/db");
}

#[test]
fn command_helper_failure_is_qf091_without_output() {
    // The command is quoted in the error, so spell the secret so that only the output has it.
    let cmd = if cfg!(windows) {
        "echo postgres://u:lea^ked@h/db & echo boom 1>&2 & exit 3"
    } else {
        "echo postgres://u:lea''ked@h/db; echo boom 1>&2; exit 3"
    };
    let e = resolve_connection(&[ConnSource::Command(cmd.into())]).unwrap_err();
    assert_eq!(e.code, "QF091");
    let all = format!("{e} {e:?}");
    assert!(all.contains("boom"), "{all}");
    assert!(!all.contains("leaked"), "{all}");

    let empty = if cfg!(windows) { "cmd /c rem" } else { "true" };
    let e = resolve_connection(&[ConnSource::Command(empty.into())]).unwrap_err();
    assert_eq!(e.code, "QF091");
}

#[test]
fn display_redacts_password() {
    let src = ConnSource::Explicit("postgres://alice:s3cr3tPw@db.example:5432/app".into());
    assert!(!format!("{src:?}").contains("s3cr3tPw"));
    let c = resolve_connection(&[src]).unwrap();
    assert_eq!(c.display_target, "db.example:5432/app");
    assert!(!c.display_target.contains("s3cr3tPw"));
    assert!(!format!("{c:?}").contains("s3cr3tPw"));
    assert!(!format!("{c:#?}").contains("s3cr3tPw"));
    assert!(!c.to_string().contains("s3cr3tPw"));

    let kv = resolve_connection(&[ConnSource::Explicit(
        "host=kvhost port=5440 dbname=kvdb user=bob password=kvSecret".into(),
    )])
    .unwrap();
    assert_eq!(kv.display_target, "kvhost:5440/kvdb");
    assert!(!format!("{kv:?}").contains("kvSecret"));
}

#[test]
fn invalid_connection_string_is_qf091_and_not_echoed() {
    let e = resolve_connection(&[ConnSource::Explicit(
        "postgres://u:hiddenPw@h:notaport/db".into(),
    )])
    .unwrap_err();
    assert_eq!(e.code, "QF091");
    assert!(!format!("{e} {e:?}").contains("hiddenPw"));
}

#[test]
fn sslmode_values() {
    for mode in ["disable", "prefer", "require", "verify-ca", "verify-full"] {
        let url = format!("postgres://u@h:5432/db?sslmode={mode}");
        let c = resolve_connection(&[ConnSource::Explicit(url)]).unwrap();
        assert_eq!(c.ssl_mode().as_str(), mode);
    }
    let e = resolve_connection(&[ConnSource::Explicit(
        "postgres://u@h:5432/db?sslmode=bogus".into(),
    )])
    .unwrap_err();
    assert_eq!(e.code, "QF091");
}

#[tokio::test]
async fn unreachable_fast_fail() {
    let c = resolve_connection(&[ConnSource::Explicit(
        "postgres://u:pw@127.0.0.1:1/db?sslmode=disable".into(),
    )])
    .unwrap();
    let timeout = Duration::from_secs(3);
    let start = Instant::now();
    let e = connect_pool(&c, 2, timeout).await.expect_err("must fail");
    assert!(start.elapsed() < timeout + Duration::from_secs(2));
    assert_eq!(e.code, "QF090");
    assert!(
        e.message.contains("cannot reach database at 127.0.0.1:1"),
        "{}",
        e.message
    );
}
