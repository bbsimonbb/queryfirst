//! Connection resolution and connect failures. No database needed. No test reads or writes the
//! process environment: resolution goes through an injected one.

use qf_pg::{ConnError, ConnSource, ResolvedConn, connect_pool, resolve_connection_with};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// An environment of exactly `pairs` (no home directory, so no password or service file).
fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    move |k: &str| map.get(k).cloned()
}

/// Resolution against an empty environment.
fn resolve_connection(sources: &[ConnSource]) -> Result<ResolvedConn, ConnError> {
    resolve_connection_with(sources, &env_of(&[]))
}

#[test]
fn explicit_wins() {
    let env = env_of(&[(
        "QF_TEST_CONN_EXPLICIT_WINS",
        "postgres://u@other:5432/otherdb",
    )]);
    let c = resolve_connection_with(
        &[
            ConnSource::Explicit("postgres://u@h1:5433/db1".into()),
            ConnSource::Env("QF_TEST_CONN_EXPLICIT_WINS".into()),
        ],
        &env,
    )
    .unwrap();
    assert_eq!(c.display_target, "h1:5433/db1");
}

#[test]
fn env_var_named() {
    let env = env_of(&[("QF_TEST_CONN_ENV_NAMED", "postgres://u@h2:5434/db2")]);
    let c = resolve_connection_with(
        &[
            ConnSource::Env("QF_TEST_CONN_ENV_NAMED_UNSET".into()),
            ConnSource::Env("QF_TEST_CONN_ENV_NAMED".into()),
            ConnSource::Explicit("postgres://u@never:5432/never".into()),
        ],
        &env,
    )
    .unwrap();
    assert_eq!(c.display_target, "h2:5434/db2");
}

#[test]
fn standard_env_database_url_then_pg_vars() {
    let pg = [
        ("DATABASE_URL", "postgres://u@h3:5435/db3"),
        ("PGHOST", "pghost"),
        ("PGPORT", "6543"),
        ("PGUSER", "pguser"),
        ("PGDATABASE", "pgdb"),
        ("PGPASSWORD", "pgsecret"),
    ];
    let std_env = [ConnSource::StandardEnv];
    let with = |extra: &[(&'static str, &'static str)]| {
        let mut all = pg.to_vec();
        all.extend_from_slice(extra);
        env_of(&all)
    };
    let c = resolve_connection_with(&std_env, &with(&[])).unwrap();
    assert_eq!(c.display_target, "h3:5435/db3");
    assert_eq!(c.ssl_mode().as_str(), "prefer");

    // PGSSLMODE applies when the connection string has no sslmode of its own.
    let env = with(&[("PGSSLMODE", "require")]);
    let c = resolve_connection_with(&std_env, &env).unwrap();
    assert_eq!(c.ssl_mode().as_str(), "require");
    let c = resolve_connection_with(
        &[ConnSource::Explicit(
            "postgres://u@h:5432/db?sslmode=disable".into(),
        )],
        &env,
    )
    .unwrap();
    assert_eq!(c.ssl_mode().as_str(), "disable");

    // PGSSLROOTCERT applies when the connection string has no sslrootcert of its own.
    let env = with(&[("PGSSLROOTCERT", "/env/root.crt")]);
    let c = resolve_connection_with(&std_env, &env).unwrap();
    assert_eq!(c.root_cert(), Some("/env/root.crt"));
    let c = resolve_connection_with(
        &[ConnSource::Explicit(
            "postgres://u@h:5432/db?sslrootcert=/url/root.crt".into(),
        )],
        &env,
    )
    .unwrap();
    assert_eq!(c.root_cert(), Some("/url/root.crt"));

    // Without DATABASE_URL, the PG* variables.
    let env = env_of(&pg[1..]);
    let c = resolve_connection_with(&std_env, &env).unwrap();
    assert_eq!(c.display_target, "pghost:6543/pgdb");
    assert!(!format!("{c:?}").contains("pgsecret"));
    assert!(!c.to_string().contains("pgsecret"));

    // Nothing configured: StandardEnv yields nothing, so the next source wins.
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
    for mode in [
        "disable",
        "prefer",
        "require",
        "no-verify",
        "verify-ca",
        "verify-full",
    ] {
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

#[test]
fn sslmode_inside_password_or_value_is_not_sslmode() {
    let c = resolve_connection(&[ConnSource::Explicit(
        "postgres://u:xsslmode=require@h:5432/db?sslmode=disable".into(),
    )])
    .unwrap();
    assert_eq!(c.ssl_mode().as_str(), "disable");
    assert_eq!(c.display_target, "h:5432/db");

    let c = resolve_connection(&[ConnSource::Explicit(
        "host=h port=5432 dbname=db user=u password='a sslmode=require' sslmode=disable".into(),
    )])
    .unwrap();
    assert_eq!(c.ssl_mode().as_str(), "disable");
    assert_eq!(c.display_target, "h:5432/db");

    let c = resolve_connection(&[ConnSource::Explicit(
        "host=h dbname=db user=u password=sslmode=require sslmode=verify-full".into(),
    )])
    .unwrap();
    assert_eq!(c.ssl_mode().as_str(), "verify-full");
}

#[test]
fn quoted_kv_sslmode() {
    let c = resolve_connection(&[ConnSource::Explicit(
        "host=h dbname=db user=u sslmode='require'".into(),
    )])
    .unwrap();
    assert_eq!(c.ssl_mode().as_str(), "require");
    let c = resolve_connection(&[ConnSource::Explicit(
        "host=h dbname=db user=u sslmode = 'verify-ca' port=5441".into(),
    )])
    .unwrap();
    assert_eq!(c.ssl_mode().as_str(), "verify-ca");
    assert_eq!(c.display_target, "h:5441/db");
}

/// A server that completes the startup handshake (trust auth) and then never answers again,
/// like a tunnel that went silent without resetting the connection.
async fn silent_server() -> std::net::SocketAddr {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut len = [0u8; 4];
                sock.read_exact(&mut len).await.unwrap();
                let mut rest = vec![0u8; u32::from_be_bytes(len) as usize - 4];
                sock.read_exact(&mut rest).await.unwrap();
                // AuthenticationOk, ReadyForQuery(idle).
                sock.write_all(&[b'R', 0, 0, 0, 8, 0, 0, 0, 0, b'Z', 0, 0, 0, 5, b'I'])
                    .await
                    .unwrap();
                let mut sink = [0u8; 1024];
                while let Ok(n) = sock.read(&mut sink).await {
                    if n == 0 {
                        break;
                    }
                }
            });
        }
    });
    addr
}

#[tokio::test]
async fn silent_connection_times_out_as_connection_error() {
    let addr = silent_server().await;
    let c = resolve_connection(&[ConnSource::Explicit(format!(
        "postgres://u@{addr}/db?sslmode=disable"
    ))])
    .unwrap();
    let pool = connect_pool(&c, 1, Duration::from_secs(5))
        .await
        .expect("handshake succeeds")
        .with_query_timeout(Duration::from_millis(300));

    let start = Instant::now();
    let err = qf_pg::describe(&pool, "SELECT 1")
        .await
        .expect_err("must time out");
    assert!(start.elapsed() < Duration::from_secs(3));
    assert!(qf_pg::is_connection_error(&err), "{err:?}");

    let start = Instant::now();
    let err = qf_pg::Catalog::load(&pool)
        .await
        .expect_err("must time out");
    assert!(start.elapsed() < Duration::from_secs(3));
    assert_eq!(err.code, "QF090");
    assert!(
        err.message
            .contains(&format!("cannot reach database at {addr}")),
        "{err}"
    );
}

#[tokio::test]
async fn accept_and_never_answer_times_out_on_connect() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((sock, _)) = listener.accept().await {
            held.push(sock);
        }
    });
    let c = resolve_connection(&[ConnSource::Explicit(format!(
        "postgres://u@{addr}/db?sslmode=disable"
    ))])
    .unwrap();
    let start = Instant::now();
    let e = connect_pool(&c, 1, Duration::from_millis(500))
        .await
        .expect_err("must time out");
    assert!(start.elapsed() < Duration::from_secs(3));
    assert_eq!(e.code, "QF090");
}

#[test]
fn read_only_appends_to_options() {
    let plain = resolve_connection(&[ConnSource::Explicit("postgres://u@h:5432/db".into())])
        .unwrap()
        .read_only();
    assert_eq!(plain.options(), Some("-c default_transaction_read_only=on"));
    let with = resolve_connection(&[ConnSource::Explicit(
        "postgres://u@h:5432/db?options=-c%20search_path%3Dapp%20-c%20default_transaction_read_only%3Doff".into(),
    )])
    .unwrap()
    .read_only();
    // Ours comes last, so it wins over a connection string asking for read-write.
    assert_eq!(
        with.options(),
        Some(
            "-c search_path=app -c default_transaction_read_only=off -c default_transaction_read_only=on"
        )
    );
    let unchanged =
        resolve_connection(&[ConnSource::Explicit("postgres://u@h:5432/db".into())]).unwrap();
    assert_eq!(unchanged.options(), None);
}

#[test]
fn redact_connection_strings() {
    use qf_pg::redact;
    assert_eq!(
        redact("node x.js postgres://app:pw@db:5432/x?sslmode=require done"),
        "node x.js postgres://*** done"
    );
    assert_eq!(
        redact("url=\"postgresql://u:p@h/db\""),
        "url=\"postgres://***\""
    );
    assert_eq!(redact("POSTGRES://U:P@H/DB"), "postgres://***");
    assert_eq!(
        redact("host=h password=s3cret user=u"),
        "host=h password=*** user=u"
    );
    assert_eq!(
        redact("password = 'two words\\' here' dbname=x"),
        "password=*** dbname=x"
    );
    assert_eq!(
        redact("PGPASSWORD=hunter2 node db.js"),
        "PGPASSWORD=*** node db.js"
    );
    assert_eq!(
        redact("nothing secret: password"),
        "nothing secret: password"
    );
    assert_eq!(redact("é postgres://x é"), "é postgres://*** é");
    // Double-quoted values, as in a shell command or a JSON-ish log line.
    assert_eq!(redact("password=\"a b\" dbname=x"), "password=*** dbname=x");
    assert_eq!(
        redact("PGPASSWORD = \"two \\\" words\" node db.js"),
        "PGPASSWORD=*** node db.js"
    );
}

fn fixture(name: &str) -> String {
    // Forward slashes: a backslash is an escape in a key=value string.
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR")).replace('\\', "/")
}

#[test]
fn sslrootcert_in_url_and_key_value() {
    let c = resolve_connection(&[ConnSource::Explicit(
        "postgres://u@h:5432/db?sslmode=verify-full&sslrootcert=%2Fcerts%2Froot%20ca.crt".into(),
    )])
    .unwrap();
    assert_eq!(c.ssl_mode().as_str(), "verify-full");
    assert_eq!(c.root_cert(), Some("/certs/root ca.crt"));
    assert_eq!(c.display_target, "h:5432/db");
    let c = resolve_connection(&[ConnSource::Explicit(
        "host=h dbname=db user=u sslrootcert='C:\\certs\\root ca.crt' sslmode=require".into(),
    )])
    .unwrap();
    assert_eq!(c.ssl_mode().as_str(), "require");
    assert_eq!(c.root_cert(), Some("C:certsroot ca.crt"));
    let c = resolve_connection(&[ConnSource::Explicit(
        "host=h dbname=db user=u sslrootcert=/a/b.crt sslmode=require".into(),
    )])
    .unwrap();
    assert_eq!(c.root_cert(), Some("/a/b.crt"));
}

#[test]
fn no_verify_is_require_without_verification() {
    let c = resolve_connection(&[ConnSource::Explicit(
        "postgres://u@h:5432/db?sslmode=no-verify".into(),
    )])
    .unwrap();
    assert_eq!(c.ssl_mode().as_str(), "no-verify");
    let c = resolve_connection(&[ConnSource::Explicit(
        "host=h dbname=db user=u sslmode=no-verify".into(),
    )])
    .unwrap();
    assert_eq!(c.ssl_mode(), qf_pg::SslMode::NoVerify);
}

#[test]
fn unsupported_option_is_named_with_a_hint() {
    for s in [
        "postgres://u:pw@h:5432/db?sslcert=/c.crt&sslmode=require",
        "host=h dbname=db user=u password=pw sslcert=/c.crt",
    ] {
        let e = resolve_connection(&[ConnSource::Explicit(s.into())]).unwrap_err();
        assert_eq!(e.code, "QF091");
        assert!(e.message.contains("`sslcert`"), "{e}");
        let hint = e.hint.as_deref().unwrap_or_default();
        assert!(hint.contains("remove `sslcert`"), "{hint}");
        assert!(hint.contains("sslrootcert"), "{hint}");
        assert!(!format!("{e} {e:?}").contains("pw@"), "{e:?}");
    }
    // Other parse errors still say what is wrong.
    let e = resolve_connection(&[ConnSource::Explicit("postgres://u@h:notaport/db".into())])
        .unwrap_err();
    assert!(e.message.contains("invalid connection string"), "{e}");
    assert!(
        e.detail.as_deref().unwrap_or_default().contains("port"),
        "{e:?}"
    );
}

#[test]
fn at_sign_in_query_value_does_not_hide_sslmode() {
    // No userinfo: the `@` is in a query value. sslmode before it must still be ours.
    let c = resolve_connection(&[ConnSource::Explicit(
        "postgres://h:5432/db?sslmode=require&application_name=me@work".into(),
    )])
    .unwrap();
    assert_eq!(c.ssl_mode().as_str(), "require");
    assert_eq!(c.display_target, "h:5432/db");
    // With userinfo too.
    let c = resolve_connection(&[ConnSource::Explicit(
        "postgres://u:p@h:5432/db?sslmode=verify-full&application_name=a@b".into(),
    )])
    .unwrap();
    assert_eq!(c.ssl_mode().as_str(), "verify-full");
    assert_eq!(c.display_target, "h:5432/db");
}

#[tokio::test]
async fn sslrootcert_is_loaded_for_verifying_modes() {
    let good = fixture("root.crt");
    // A good root file: TLS is set up, and the failure is reaching the (closed) port.
    for mode in ["verify-full", "verify-ca", "require"] {
        let c = resolve_connection(&[ConnSource::Explicit(format!(
            "host=127.0.0.1 port=1 dbname=db user=u sslmode={mode} sslrootcert='{good}'"
        ))])
        .unwrap();
        let e = connect_pool(&c, 1, Duration::from_secs(3))
            .await
            .unwrap_err();
        assert_eq!(e.code, "QF090", "{mode}: {e}");
    }
    // A missing or non-PEM file is a configuration error naming the file, before connecting.
    let missing = fixture("no-such-root.crt");
    let not_pem = fixture("schema.sql");
    for (mode, path) in [("verify-full", &missing), ("require", &not_pem)] {
        let c = resolve_connection(&[ConnSource::Explicit(format!(
            "host=127.0.0.1 port=1 dbname=db user=u sslmode={mode} sslrootcert='{path}'"
        ))])
        .unwrap();
        let e = connect_pool(&c, 1, Duration::from_secs(3))
            .await
            .unwrap_err();
        assert_eq!(e.code, "QF091", "{mode}: {e}");
        assert!(e.message.contains("sslrootcert"), "{e}");
        assert!(e.message.contains(path.as_str()), "{e}");
    }
    // Modes that never verify do not read it.
    for mode in ["no-verify", "prefer", "disable"] {
        let c = resolve_connection(&[ConnSource::Explicit(format!(
            "host=127.0.0.1 port=1 dbname=db user=u sslmode={mode} sslrootcert='{missing}'"
        ))])
        .unwrap();
        let e = connect_pool(&c, 1, Duration::from_secs(3))
            .await
            .unwrap_err();
        assert_eq!(e.code, "QF090", "{mode}: {e}");
    }
}

#[test]
fn helper_command_and_stderr_are_redacted() {
    // The command text names a password; the helper prints a URL on stderr and fails.
    let cmd = if cfg!(windows) {
        "set PGPASSWORD=cmdsecret& echo bad postgres://u:errsecret@h/db 1>&2 & exit 3"
    } else {
        "PGPASSWORD=cmdsecret; echo bad postgres://u:errsecret@h/db 1>&2; exit 3"
    };
    let src = ConnSource::Command(cmd.into());
    let e = resolve_connection(std::slice::from_ref(&src)).unwrap_err();
    assert_eq!(e.code, "QF091");
    let all = format!("{e} {e:?} {src:?}");
    assert!(all.contains("bad postgres://***"), "{all}");
    assert!(all.contains("PGPASSWORD=***"), "{all}");
    for secret in ["cmdsecret", "errsecret"] {
        assert!(!all.contains(secret), "{all}");
    }

    // A helper whose output does not parse: the origin quotes the (redacted) command.
    let bad = echo("password=outsecret nonsense");
    let e =
        resolve_connection(&[ConnSource::Command(format!("{bad} password=cmdpw"))]).unwrap_err();
    let all = format!("{e} {e:?}");
    assert!(
        !all.contains("cmdpw") && !all.contains("outsecret"),
        "{all}"
    );
}
