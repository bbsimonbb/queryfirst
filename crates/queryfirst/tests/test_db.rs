//! `qf test` end to end against a real Postgres: generate against the fixture schema ("schema
//! A"), bundle, alter the schema, then run `qf test --manifest` from a directory that holds only
//! the manifest (as in a deployment image).
//!
//! Needs `QF_TEST_DATABASE_URL`; fails (never skips) without one. Each test gets its own uniquely
//! named schema, selected through `search_path` in the connection string, dropped afterwards.
#![cfg(feature = "db-tests")]

use assert_cmd::Command;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime};

const FIXTURE: &str = include_str!("../../qf-pg/tests/fixtures/schema.sql");

fn database_url() -> String {
    std::env::var("QF_TEST_DATABASE_URL")
        .expect("QF_TEST_DATABASE_URL must be set when running with --features db-tests")
}

fn unique_schema() -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    let nanos = SystemTime::now()
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

/// `url` with libpq `options`: `-c name=value` for each setting.
fn with_options(url: &str, settings: &[(&str, &str)]) -> String {
    let opts: Vec<String> = settings
        .iter()
        .map(|(k, v)| format!("-c%20{k}%3D{v}"))
        .collect();
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}options={}", opts.join("%20"))
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// A temp schema with the fixture; dropped on drop (also when the test panics).
struct Db {
    schema: String,
}

impl Db {
    fn new() -> Db {
        let schema = unique_schema();
        let db = Db { schema };
        db.exec(&format!(
            "CREATE SCHEMA {s}; SET search_path TO {s};\n{FIXTURE}",
            s = db.schema
        ));
        db
    }

    /// The connection string for `qf`, with this schema on the search path.
    fn url(&self) -> String {
        self.url_with(&[])
    }

    fn url_with(&self, extra: &[(&str, &str)]) -> String {
        let mut settings = vec![("search_path", self.schema.as_str())];
        settings.extend_from_slice(extra);
        with_options(&database_url(), &settings)
    }

    fn client(&self) -> (tokio::runtime::Runtime, tokio_postgres::Client) {
        let rt = runtime();
        let client = rt.block_on(async {
            let (client, conn) = tokio::time::timeout(
                Duration::from_secs(10),
                tokio_postgres::connect(&database_url(), tokio_postgres::NoTls),
            )
            .await
            .expect("timed out connecting to QF_TEST_DATABASE_URL")
            .expect("cannot connect to QF_TEST_DATABASE_URL");
            tokio::spawn(conn);
            client
        });
        (rt, client)
    }

    /// Run SQL in this schema (the "alter the DB" step).
    fn exec(&self, sql: &str) {
        let (rt, client) = self.client();
        rt.block_on(async {
            client
                .batch_execute(&format!("SET search_path TO {};\n{sql}", self.schema))
                .await
                .unwrap_or_else(|e| panic!("{sql}: {e}"));
        });
    }

    /// Rows inserted, updated and deleted in this schema's tables, per the statistics views.
    fn tuple_writes(&self) -> i64 {
        let (rt, client) = self.client();
        rt.block_on(async {
            client
                .batch_execute("SELECT pg_stat_clear_snapshot()")
                .await
                .unwrap();
            let row = client
                .query_one(
                    "SELECT coalesce(sum(n_tup_ins + n_tup_upd + n_tup_del), 0)::int8 \
                     FROM pg_stat_user_tables WHERE schemaname = $1",
                    &[&self.schema],
                )
                .await
                .unwrap();
            row.get(0)
        })
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        let schema = self.schema.clone();
        runtime().block_on(async {
            if let Ok((client, conn)) =
                tokio_postgres::connect(&database_url(), tokio_postgres::NoTls).await
            {
                tokio::spawn(conn);
                let _ = client
                    .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
                    .await;
            }
        });
    }
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Out {
    fn all(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

/// `qf` in `dir` with only `url` (if any) as `DATABASE_URL`. Asserts that neither the
/// connection string nor its password is ever printed.
fn qf(dir: &Path, url: Option<&str>, args: &[&str]) -> Out {
    let mut c = Command::cargo_bin("qf").unwrap();
    c.current_dir(dir);
    for v in [
        "DATABASE_URL",
        "PGHOST",
        "PGPORT",
        "PGUSER",
        "PGDATABASE",
        "PGPASSWORD",
        "PGSERVICE",
        "PGSERVICEFILE",
        "PGSYSCONFDIR",
        "PGPASSFILE",
        "PGCONNECT_TIMEOUT",
        "PGAPPNAME",
        "PGSSLMODE",
    ] {
        c.env_remove(v);
    }
    if let Some(url) = url {
        c.env("DATABASE_URL", url);
    }
    let o = c.args(args).output().unwrap();
    let out = Out {
        code: o.status.code().unwrap_or(-1),
        stdout: String::from_utf8(o.stdout).unwrap(),
        stderr: String::from_utf8(o.stderr).unwrap(),
    };
    let all = out.all();
    if let (_, Some(pw)) = target_and_password(&database_url()) {
        assert!(
            pw.is_empty() || !all.contains(&pw),
            "password printed: {all}"
        );
    }
    assert!(
        !all.contains("postgres://"),
        "connection string printed: {all}"
    );
    out
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

const INVOICE: &str = "/* @name getInvoice */\nSELECT i.id, i.total, i.notes, c.email\nFROM invoices i\nJOIN customers c ON c.id = i.customer_id\nWHERE i.id = :id\n";
const ADDRESSES: &str =
    "/* @name addresses */\nSELECT * FROM addresses WHERE customer_id = :customerId\n";

/// A deployment: the project is generated and bundled against schema A, and the manifest is
/// copied alone into a fresh directory (no queryfirst.json, no sources, no metas).
struct Deployed {
    project: tempfile::TempDir,
    image: tempfile::TempDir,
}

impl Deployed {
    fn manifest(&self) -> &Path {
        self.image.path()
    }
}

fn deploy(db: &Db, config: &str) -> Deployed {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    write(root, "queryfirst.json", config);
    write(root, "app/get-invoice.psql", INVOICE);
    write(root, "app/addresses.psql", ADDRESSES);
    let g = qf(root, Some(&db.url()), &["generate"]);
    assert_eq!(g.code, 0, "{}", g.stderr);
    let b = qf(root, None, &["bundle"]);
    assert_eq!(b.code, 0, "{}", b.stderr);
    let image = tempfile::tempdir().unwrap();
    std::fs::copy(
        root.join("queryfirst.manifest.json"),
        image.path().join("queryfirst.manifest.json"),
    )
    .unwrap();
    Deployed { project, image }
}

fn test_manifest(dir: &Path, url: Option<&str>, extra: &[&str]) -> Out {
    let mut args = vec!["test", "--manifest", "queryfirst.manifest.json"];
    args.extend_from_slice(extra);
    qf(dir, url, &args)
}

fn json(out: &Out) -> serde_json::Value {
    serde_json::from_str(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", out.stdout))
}

fn codes(out: &Out) -> Vec<String> {
    json(out)["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["code"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn unchanged_schema_passes() {
    let db = Db::new();
    let d = deploy(&db, "{}\n");
    let out = test_manifest(d.manifest(), Some(&db.url()), &[]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stderr
            .contains("Queryfirst test: 2 queries, 0 with differences"),
        "{}",
        out.stderr
    );
}

#[test]
fn tree_mode_passes_unchanged() {
    let db = Db::new();
    let d = deploy(&db, "{}\n");
    let out = qf(d.project.path(), Some(&db.url()), &["test"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
}

#[test]
fn add_column_passes_with_note() {
    let db = Db::new();
    let d = deploy(&db, "{}\n");
    db.exec("ALTER TABLE addresses ADD COLUMN country text");
    let out = test_manifest(d.manifest(), Some(&db.url()), &["--json"]);
    assert_eq!(out.code, 0, "{}", out.stdout);
    assert_eq!(codes(&out), ["QF043"]);
    let v = json(&out);
    assert_eq!(v["diagnostics"][0]["severity"], "note");
    assert_eq!(v["diagnostics"][0]["file"], "app/addresses.psql");
    assert!(
        v["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("column \"country\" text added"),
        "{v}"
    );
}

#[test]
fn drop_column_fails_qf040() {
    let db = Db::new();
    let d = deploy(&db, "{}\n");
    db.exec("ALTER TABLE addresses DROP COLUMN city");
    let out = test_manifest(d.manifest(), Some(&db.url()), &[]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr
            .contains("app/addresses.psql: error[QF040]: addresses: column \"city\" missing"),
        "{}",
        out.stderr
    );
}

#[test]
fn widen_numeric_fails_qf041_message() {
    let db = Db::new();
    let d = deploy(&db, "{}\n");
    db.exec("ALTER TABLE invoices ALTER COLUMN total TYPE numeric(14,2)");
    let out = test_manifest(d.manifest(), Some(&db.url()), &[]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr
            .contains("error[QF041]: getInvoice: column \"total\" numeric(12,2) → numeric(14,2)"),
        "{}",
        out.stderr
    );
}

#[test]
fn drop_not_null_fails_qf042() {
    let db = Db::new();
    let d = deploy(&db, "{}\n");
    db.exec("ALTER TABLE invoices ALTER COLUMN total DROP NOT NULL");
    let out = test_manifest(d.manifest(), Some(&db.url()), &[]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr
            .contains("error[QF042]: getInvoice: column \"total\" became nullable"),
        "{}",
        out.stderr
    );
}

#[test]
fn prepare_failure_qf046() {
    let db = Db::new();
    let d = deploy(&db, "{}\n");
    db.exec("ALTER TABLE invoices DROP COLUMN notes");
    let out = test_manifest(d.manifest(), Some(&db.url()), &["--json"]);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert_eq!(codes(&out), ["QF046"]);
}

#[test]
fn strict_fails_on_note() {
    let db = Db::new();
    let d = deploy(&db, "{}\n");
    db.exec("ALTER TABLE addresses ADD COLUMN country text");
    let out = test_manifest(d.manifest(), Some(&db.url()), &["--strict"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("note[QF043]"), "{}", out.stderr);
}

#[test]
fn timezone_check() {
    let db = Db::new();
    // The manifest carries `timestamp: "utc"` from the project's queryfirst.json.
    let d = deploy(&db, "{\"timestamp\": \"utc\"}\n");
    // A per-session setting, so concurrent tests on the same database are not affected; the
    // catalog reads the session `TimeZone`, which is where a database default would show too.
    let paris = db.url_with(&[("TimeZone", "Europe/Paris")]);
    let out = test_manifest(d.manifest(), Some(&paris), &[]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("error[QF048]"), "{}", out.stderr);
    assert!(out.stderr.contains("Europe/Paris"), "{}", out.stderr);

    let utc = db.url_with(&[("TimeZone", "UTC")]);
    let out = test_manifest(d.manifest(), Some(&utc), &[]);
    assert_eq!(out.code, 0, "{}", out.stderr);
}

#[test]
fn command_helper_connection() {
    let db = Db::new();
    let d = deploy(&db, "{}\n");
    let dir = d.manifest();
    // The helper prints the URL on stdout, as a project's secrets-loading script would.
    write(dir, "db-url.txt", &db.url());
    let cat = if cfg!(windows) { "type" } else { "cat" };
    write(
        dir,
        "queryfirst.json",
        &format!("{{\"test\": {{\"connection\": {{\"command\": \"{cat} db-url.txt\"}}}}}}\n"),
    );
    let out = test_manifest(dir, None, &[]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let out = test_manifest(dir, None, &["--json"]);
    assert_eq!(out.code, 0, "{}", out.stdout);
    // `qf` asserts the URL and password never appear in its output.
}

#[test]
fn read_only() {
    let db = Db::new();
    let d = deploy(&db, "{}\n");
    // Let the statistics of setup (fixture inserts) settle first.
    std::thread::sleep(Duration::from_millis(1500));
    let before = db.tuple_writes();
    for _ in 0..3 {
        let out = test_manifest(d.manifest(), Some(&db.url()), &[]);
        assert_eq!(out.code, 0, "{}", out.stderr);
    }
    // `qf` has exited, so its sessions flushed their statistics.
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(db.tuple_writes(), before, "qf test wrote to a table");

    // Control: a real write does show up, so the measurement works.
    db.exec("INSERT INTO drafts (body) VALUES ('x')");
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        db.tuple_writes() > before,
        "statistics are not being collected"
    );
}

/// What the human summary must name for `url` (`host:port/db`), and its password, which it must
/// never show.
fn target_and_password(url: &str) -> (String, Option<String>) {
    let c: tokio_postgres::Config = url.parse().expect("test URL parses");
    let host = match &c.get_hosts()[0] {
        tokio_postgres::config::Host::Tcp(h) => h.clone(),
        #[allow(unreachable_patterns)]
        other => format!("{other:?}"),
    };
    let port = c.get_ports().first().copied().unwrap_or(5432);
    let db = c.get_dbname().map(str::to_string).unwrap_or_default();
    let pw = c
        .get_password()
        .map(|p| String::from_utf8_lossy(p).into_owned());
    (format!("{host}:{port}/{db}"), pw)
}

fn assert_names_target(url: &str, stderr: &str) {
    let (target, pw) = target_and_password(url);
    assert!(
        stderr.contains(&format!("database: {target}")),
        "no target line in: {stderr}"
    );
    if let Some(pw) = pw.filter(|p| !p.is_empty()) {
        // Otherwise the check below cannot tell a printed password from the target line.
        assert!(
            !url.replace(&format!(":{pw}@"), "@").contains(&pw),
            "QF_TEST_DATABASE_URL: use a password that is not part of the user, host or database name"
        );
        assert!(!stderr.contains(&pw), "password printed");
    }
    assert!(!stderr.contains("postgres://"), "connection string printed");
}

#[test]
fn summary_names_the_database_without_password() {
    let db = Db::new();
    let d = deploy(&db, "{}\n");
    let out = test_manifest(d.manifest(), Some(&db.url()), &[]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_names_target(&db.url(), &out.stderr);
}
