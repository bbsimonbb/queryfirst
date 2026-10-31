//! Resolution against an injected environment: service files, the password file and the
//! password fallback. No test reads or writes the process environment.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use super::*;

/// An environment of exactly `pairs`, with the per-user directory (`APPDATA` / `HOME`) pointing
/// at `home` so no test sees the real `.pgpass` or `.pg_service.conf`.
fn env_in(home: &Path, pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
    let mut map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let home = home.display().to_string();
    map.entry("APPDATA".into()).or_insert_with(|| home.clone());
    map.entry("HOME".into()).or_insert(home);
    move |k: &str| map.get(k).cloned()
}

/// Where libpq looks for per-user files under `home` (see [`env_in`]).
fn user_dir(home: &Path) -> std::path::PathBuf {
    let d = if cfg!(windows) {
        home.join("postgresql")
    } else {
        home.to_path_buf()
    };
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_private(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn resolve(s: &str, env: &dyn Fn(&str) -> Option<String>) -> Result<ResolvedConn, ConnError> {
    resolve_connection_with(&[ConnSource::Explicit(s.into())], env)
}

fn password(c: &ResolvedConn) -> Option<String> {
    c.config
        .get_password()
        .map(|p| String::from_utf8_lossy(p).into_owned())
}

fn path_str(p: &Path) -> String {
    // Forward slashes: a backslash is an escape in key=value strings.
    p.display().to_string().replace('\\', "/")
}

#[test]
fn url_without_password_uses_pgpassword() {
    let home = tempfile::tempdir().unwrap();
    let env = env_in(home.path(), &[("PGPASSWORD", "envpw")]);
    let c = resolve("postgresql://u@127.0.0.1:25071/db", &env).unwrap();
    assert_eq!(password(&c).as_deref(), Some("envpw"));
    assert_eq!(c.display_target, "127.0.0.1:25071/db");
    assert!(!format!("{c:?} {c}").contains("envpw"));
}

#[test]
fn named_env_var_without_password_uses_pgpassword() {
    let home = tempfile::tempdir().unwrap();
    let env = env_in(
        home.path(),
        &[
            ("QUERYFIRST_DATABASE_URL", "postgresql://u@h:5432/db"),
            ("PGPASSWORD", "envpw"),
        ],
    );
    let c = resolve_connection_with(&[ConnSource::Env("QUERYFIRST_DATABASE_URL".into())], &env)
        .unwrap();
    assert_eq!(password(&c).as_deref(), Some("envpw"));
}

#[test]
fn url_with_password_ignores_pgpassword_and_pgpass() {
    let home = tempfile::tempdir().unwrap();
    write_private(
        &user_dir(home.path()).join(pgpass_name()),
        "*:*:*:*:filepw\n",
    );
    let env = env_in(home.path(), &[("PGPASSWORD", "envpw")]);
    let c = resolve("postgres://u:urlpw@h:5432/db", &env).unwrap();
    assert_eq!(password(&c).as_deref(), Some("urlpw"));
    let c = resolve("host=h user=u dbname=db password=kvpw", &env).unwrap();
    assert_eq!(password(&c).as_deref(), Some("kvpw"));
}

fn pgpass_name() -> &'static str {
    if cfg!(windows) {
        "pgpass.conf"
    } else {
        ".pgpass"
    }
}

#[test]
fn pgpassword_beats_pgpass() {
    let home = tempfile::tempdir().unwrap();
    write_private(
        &user_dir(home.path()).join(pgpass_name()),
        "*:*:*:*:filepw\n",
    );
    let env = env_in(home.path(), &[("PGPASSWORD", "envpw")]);
    let c = resolve("postgres://u@h/db", &env).unwrap();
    assert_eq!(password(&c).as_deref(), Some("envpw"));
}

#[test]
fn default_pgpass_file_matches_the_resolved_target() {
    let home = tempfile::tempdir().unwrap();
    write_private(
        &user_dir(home.path()).join(pgpass_name()),
        "# host:port:db:user:password\n\
         h:6000:db:u:wrongport\n\
         h:5432:other:u:wrongdb\n\
         h:5432:db:u:right\n\
         localhost:5432:u:u:defaults\n\
         *:*:*:*:fallback\n",
    );
    let env = env_in(home.path(), &[]);
    // The URL has no port: the default 5432 is what is matched.
    let c = resolve("postgres://u@h/db", &env).unwrap();
    assert_eq!(password(&c).as_deref(), Some("right"));
    // No host, port or database: localhost, 5432 and the user name.
    let c = resolve("user=u", &env).unwrap();
    assert_eq!(password(&c).as_deref(), Some("defaults"));
    let c = resolve("postgres://x@elsewhere:1/y", &env).unwrap();
    assert_eq!(password(&c).as_deref(), Some("fallback"));
}

#[test]
fn pgpassfile_and_passfile_choose_the_file() {
    let home = tempfile::tempdir().unwrap();
    write_private(
        &user_dir(home.path()).join(pgpass_name()),
        "*:*:*:*:default\n",
    );
    let a = home.path().join("a.pgpass");
    let b = home.path().join("b.pgpass");
    write_private(&a, "*:*:*:*:from-a\n");
    write_private(&b, "*:*:*:*:from-b\n");
    let env = env_in(home.path(), &[("PGPASSFILE", &a.display().to_string())]);
    let c = resolve("postgres://u@h/db", &env).unwrap();
    assert_eq!(password(&c).as_deref(), Some("from-a"));
    let c = resolve(&format!("host=h user=u passfile='{}'", path_str(&b)), &env).unwrap();
    assert_eq!(password(&c).as_deref(), Some("from-b"));
    // `passfile` is ours, never passed on to tokio-postgres as an unknown option.
    let url = format!(
        "postgres://u@h/db?passfile={}",
        path_str(&b).replace(':', "%3A")
    );
    let c = resolve(&url, &env).unwrap();
    assert_eq!(password(&c).as_deref(), Some("from-b"));
}

#[test]
fn missing_pgpass_is_no_password() {
    let home = tempfile::tempdir().unwrap();
    let env = env_in(home.path(), &[]);
    let c = resolve("postgres://u@h/db", &env).unwrap();
    assert_eq!(password(&c), None);
    let env = env_in(home.path(), &[("PGPASSFILE", "/no/such/file")]);
    let c = resolve("postgres://u@h/db", &env).unwrap();
    assert_eq!(password(&c), None);
}

#[test]
fn empty_url_password_falls_back() {
    let home = tempfile::tempdir().unwrap();
    let env = env_in(home.path(), &[("PGPASSWORD", "envpw")]);
    let c = resolve("postgres://u:@h/db", &env).unwrap();
    assert_eq!(password(&c).as_deref(), Some("envpw"));
}

const SERVICES: &str = "\
# Queryfirst test services
[dev]
host=svc.example
port=6000
dbname=svcdb
user=svcuser
sslmode=require
application_name=from-service

[withpw]
host=svc.example
user=svcuser
password=svcpw
";

#[test]
fn pgservice_fills_the_connection() {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("services.conf");
    write_private(&file, SERVICES);
    let env = env_in(
        home.path(),
        &[
            ("PGSERVICEFILE", &file.display().to_string()),
            ("PGSERVICE", "dev"),
        ],
    );
    // PGSERVICE alone makes the PG* environment a source.
    let c = resolve_connection_with(&[ConnSource::StandardEnv], &env).unwrap();
    assert_eq!(c.display_target, "svc.example:6000/svcdb");
    assert_eq!(c.config.get_user(), Some("svcuser"));
    assert_eq!(c.ssl_mode(), SslMode::Require);
    assert_eq!(c.config.get_application_name(), Some("from-service"));
    // It also completes an explicit string, which wins where it says something.
    let c = resolve("postgres://me@other/mydb?sslmode=disable", &env).unwrap();
    assert_eq!(c.display_target, "other:6000/mydb");
    assert_eq!(c.config.get_user(), Some("me"));
    assert_eq!(c.ssl_mode(), SslMode::Disable);
}

#[test]
fn service_key_in_string_beats_pgservice() {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("services.conf");
    write_private(&file, SERVICES);
    let env = env_in(
        home.path(),
        &[
            ("PGSERVICEFILE", &file.display().to_string()),
            ("PGSERVICE", "withpw"),
        ],
    );
    let c = resolve("service=dev", &env).unwrap();
    assert_eq!(c.display_target, "svc.example:6000/svcdb");
    let c = resolve("postgresql://?service=dev", &env).unwrap();
    assert_eq!(c.display_target, "svc.example:6000/svcdb");
    let c = resolve("postgresql:///x?service=dev", &env).unwrap();
    assert_eq!(c.display_target, "svc.example:6000/x");
}

#[test]
fn service_beats_environment_as_in_libpq() {
    // libpq fills unset options from the service first, then from the environment.
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("services.conf");
    write_private(&file, SERVICES);
    let env = env_in(
        home.path(),
        &[
            ("PGSERVICEFILE", &file.display().to_string()),
            ("PGHOST", "envhost"),
            ("PGDATABASE", "envdb"),
            ("PGPORT", "7000"),
            ("PGPASSWORD", "envpw"),
            ("PGSSLMODE", "disable"),
        ],
    );
    let c = resolve("service=withpw", &env).unwrap();
    // host and password from the service; port and database (not in it) from the environment.
    assert_eq!(c.display_target, "svc.example:7000/envdb");
    assert_eq!(password(&c).as_deref(), Some("svcpw"));
    assert_eq!(c.ssl_mode(), SslMode::Disable);
    let c = resolve("service=dev", &env).unwrap();
    assert_eq!(c.ssl_mode(), SslMode::Require);
    assert_eq!(password(&c).as_deref(), Some("envpw"));
}

#[test]
fn service_file_lookup_order() {
    let home = tempfile::tempdir().unwrap();
    let sys = tempfile::tempdir().unwrap();
    write_private(
        &user_dir(home.path()).join(".pg_service.conf"),
        "[dev]\nhost=user-file\n",
    );
    write_private(
        &sys.path().join("pg_service.conf"),
        "[dev]\nhost=system-file\n[sysonly]\nhost=system-only\n",
    );
    let sysdir = sys.path().display().to_string();
    let env = env_in(home.path(), &[("PGSYSCONFDIR", &sysdir)]);
    assert_eq!(
        resolve("service=dev", &env).unwrap().host_port(),
        "user-file:5432"
    );
    // Not in the user file: the system file.
    assert_eq!(
        resolve("service=sysonly", &env).unwrap().host_port(),
        "system-only:5432"
    );
    // PGSERVICEFILE replaces the per-user file.
    let custom = home.path().join("custom.conf");
    write_private(&custom, "[dev]\nhost=custom-file\n");
    let env = env_in(
        home.path(),
        &[
            ("PGSYSCONFDIR", &sysdir),
            ("PGSERVICEFILE", &custom.display().to_string()),
        ],
    );
    assert_eq!(
        resolve("service=dev", &env).unwrap().host_port(),
        "custom-file:5432"
    );
}

#[test]
fn unknown_service_is_qf091_naming_service_and_files() {
    let home = tempfile::tempdir().unwrap();
    let sys = tempfile::tempdir().unwrap();
    let custom = home.path().join("custom.conf");
    write_private(&custom, SERVICES);
    let env = env_in(
        home.path(),
        &[
            ("PGSERVICEFILE", &custom.display().to_string()),
            ("PGSYSCONFDIR", &sys.path().display().to_string()),
            ("PGSERVICE", "nosuch"),
        ],
    );
    let e = resolve_connection_with(&[ConnSource::StandardEnv], &env).unwrap_err();
    assert_eq!(e.code, "QF091");
    assert!(e.message.contains("`nosuch`"), "{e}");
    let detail = e.detail.clone().unwrap_or_default();
    assert!(detail.contains(&custom.display().to_string()), "{detail}");
    assert!(
        detail.contains(&sys.path().join("pg_service.conf").display().to_string()),
        "{detail}"
    );
    assert!(!format!("{e:?}").contains("svcpw"));
}

#[test]
fn unsupported_and_bad_service_lines_never_echo_content() {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("s.conf");
    write_private(
        &file,
        "[a]\nhost=h\nsslcert=/c.crt\n[b]\nhunter2secret\n[c]\nhunter2=typo\n[d]\nservice=a\n",
    );
    let env = env_in(
        home.path(),
        &[("PGSERVICEFILE", &file.display().to_string())],
    );
    let e = resolve("service=a", &env).unwrap_err();
    assert_eq!(e.code, "QF091");
    assert!(e.message.contains("`sslcert`"), "{e}");
    assert!(e.message.contains("line 3"), "{e}");
    assert!(
        e.hint
            .as_deref()
            .unwrap_or_default()
            .contains("sslrootcert")
    );
    for svc in ["b", "c"] {
        let e = resolve(&format!("service={svc}"), &env).unwrap_err();
        assert_eq!(e.code, "QF091");
        assert!(e.message.contains("syntax error"), "{e}");
        assert!(!format!("{e} {e:?}").contains("hunter2"), "{e:?}");
    }
    let e = resolve("service=d", &env).unwrap_err();
    assert!(e.message.contains("nested service"), "{e}");
}

#[test]
fn pg_env_extras() {
    let home = tempfile::tempdir().unwrap();
    let env = env_in(
        home.path(),
        &[
            ("PGPORT", "6543"),
            ("PGAPPNAME", "qf-app"),
            ("PGCONNECT_TIMEOUT", "9"),
            ("PGSSLROOTCERT", "/env/root.crt"),
        ],
    );
    // A URL host without a port leaves the port to PGPORT, as in libpq.
    let c = resolve("postgres://u@h/db", &env).unwrap();
    assert_eq!(c.display_target, "h:6543/db");
    assert_eq!(c.config.get_application_name(), Some("qf-app"));
    assert_eq!(
        c.config.get_connect_timeout(),
        Some(&Duration::from_secs(9))
    );
    assert_eq!(c.root_cert(), Some("/env/root.crt"));
    let env = env_in(home.path(), &[("PGCONNECT_TIMEOUT", "soon")]);
    assert_eq!(resolve("host=h", &env).unwrap_err().code, "QF091");
}

#[test]
fn multi_host_url_and_ipv6() {
    let home = tempfile::tempdir().unwrap();
    let env = env_in(home.path(), &[]);
    let c = resolve("postgres://u@[::1]:5433/db", &env).unwrap();
    assert_eq!(c.display_target, "::1:5433/db");
    let c = resolve("postgres://u@h1:5433,h2:5434/db", &env).unwrap();
    assert_eq!(c.config.get_hosts().len(), 2);
    assert_eq!(c.config.get_ports(), &[5433, 5434]);
}

#[test]
fn full_url_ignores_other_pg_vars_and_only_gains_a_password() {
    let home = tempfile::tempdir().unwrap();
    let others = [
        ("PGHOST", "envhost"),
        ("PGPORT", "7777"),
        ("PGDATABASE", "envdb"),
        ("PGUSER", "envuser"),
    ];
    let url = "postgresql://app@127.0.0.1:25071/appdb";
    let mut with_pw = others.to_vec();
    with_pw.push(("PGPASSWORD", "envpw"));
    let c = resolve(url, &env_in(home.path(), &with_pw)).unwrap();
    assert_eq!(c.display_target, "127.0.0.1:25071/appdb");
    assert_eq!(c.config.get_user(), Some("app"));
    assert_eq!(password(&c).as_deref(), Some("envpw"));

    write_private(
        &user_dir(home.path()).join(pgpass_name()),
        "envhost:7777:envdb:envuser:wrong\n127.0.0.1:25071:appdb:app:filepw\n",
    );
    let c = resolve(url, &env_in(home.path(), &others)).unwrap();
    assert_eq!(c.display_target, "127.0.0.1:25071/appdb");
    assert_eq!(c.config.get_user(), Some("app"));
    assert_eq!(password(&c).as_deref(), Some("filepw"));
}

#[test]
fn at_sign_in_query_without_path_does_not_hide_options() {
    let home = tempfile::tempdir().unwrap();
    let env = env_in(home.path(), &[("PGUSER", "envuser")]);
    let c = resolve(
        "postgres://h:5432?sslmode=require&application_name=a@b",
        &env,
    )
    .unwrap();
    assert_eq!(c.host_port(), "h:5432");
    assert_eq!(c.ssl_mode(), SslMode::Require);
    assert_eq!(c.config.get_user(), Some("envuser"));
    assert_eq!(c.config.get_application_name(), Some("a@b"));
}

#[test]
fn malformed_url_query_key_is_rejected() {
    let home = tempfile::tempdir().unwrap();
    let env = env_in(home.path(), &[]);
    for url in [
        "postgres://u@h/db?bad%20key=1",
        "postgres://u@h/db?Host=x",
        "postgres://u@h/db?a'b=1",
    ] {
        let e = resolve(url, &env).unwrap_err();
        assert_eq!(e.code, "QF091", "{url}");
    }
}

#[test]
fn invalid_service_values_name_the_line_not_the_value() {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("s.conf");
    write_private(
        &file,
        "[a]\nsslmode=hunter2a\n[b]\nport=hunter2b\n[c]\nconnect_timeout=hunter2c\n\
         [d]\nservicefile=/x\n[e]\nsslkeylogfile=/x\n",
    );
    let env = env_in(
        home.path(),
        &[("PGSERVICEFILE", &file.display().to_string())],
    );
    for (svc, key, line) in [
        ("a", "sslmode", 2),
        ("b", "port", 4),
        ("c", "connect_timeout", 6),
    ] {
        let e = resolve(&format!("service={svc}"), &env).unwrap_err();
        assert_eq!(e.code, "QF091");
        assert!(e.message.contains(&format!("`{key}`")), "{e}");
        assert!(e.message.contains(&format!("line {line}")), "{e}");
        assert!(!format!("{e} {e:?}").contains("hunter2"), "{e:?}");
    }
    for (svc, key) in [("d", "servicefile"), ("e", "sslkeylogfile")] {
        let e = resolve(&format!("service={svc}"), &env).unwrap_err();
        assert!(
            e.message
                .contains(&format!("unsupported connection option `{key}`")),
            "{e}"
        );
    }
}

#[test]
fn empty_service_key_ignores_pgservice() {
    let home = tempfile::tempdir().unwrap();
    let env = env_in(home.path(), &[("PGSERVICE", "nosuch")]);
    assert_eq!(resolve("host=h", &env).unwrap_err().code, "QF091");
    let c = resolve("host=h service=", &env).unwrap();
    assert_eq!(c.host_port(), "h:5432");
    let c = resolve("postgres://u@h/db?service=", &env).unwrap();
    assert_eq!(c.host_port(), "h:5432");
}
