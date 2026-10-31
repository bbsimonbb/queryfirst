//! `qf describe` end to end against a real Postgres.
//!
//! Needs `QF_TEST_DATABASE_URL`; fails (never skips) without one. Each test loads qf-pg's fixture
//! schema into its own uniquely named schema (selected through `search_path` in the connection
//! string given to `qf` as `DATABASE_URL`) and drops it afterwards.
#![cfg(feature = "db-tests")]

use assert_cmd::Command;
use qf_meta::{Status, from_json};
use std::collections::BTreeMap;
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
        "qf_desc_{}_{}_{}",
        std::process::id(),
        nanos,
        N.fetch_add(1, Ordering::Relaxed)
    )
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// A temp schema with the fixture; dropped on drop (also when the test panics).
struct Db {
    url: String,
    schema: String,
}

impl Db {
    fn new() -> Db {
        let base = database_url();
        let schema = unique_schema();
        runtime().block_on(async {
            let (client, conn) = tokio::time::timeout(
                Duration::from_secs(10),
                tokio_postgres::connect(&base, tokio_postgres::NoTls),
            )
            .await
            .expect("timed out connecting to QF_TEST_DATABASE_URL")
            .expect("cannot connect to QF_TEST_DATABASE_URL");
            tokio::spawn(conn);
            client
                .batch_execute(&format!(
                    "CREATE SCHEMA {schema}; SET search_path TO {schema};\n{FIXTURE}"
                ))
                .await
                .expect("load fixture schema");
        });
        let sep = if base.contains('?') { '&' } else { '?' };
        Db {
            url: format!("{base}{sep}options=-c%20search_path%3D{schema}"),
            schema,
        }
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        let base = database_url();
        let schema = self.schema.clone();
        runtime().block_on(async {
            if let Ok((client, conn)) = tokio_postgres::connect(&base, tokio_postgres::NoTls).await
            {
                tokio::spawn(conn);
                let _ = client
                    .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
                    .await;
            }
        });
    }
}

fn qf(project: &Path, url: &str) -> Command {
    let mut c = Command::cargo_bin("qf").unwrap();
    c.current_dir(project).arg("--project").arg(project);
    for v in [
        "PGHOST",
        "PGPORT",
        "PGUSER",
        "PGDATABASE",
        "PGPASSWORD",
        "PGSSLMODE",
    ] {
        c.env_remove(v);
    }
    c.env("DATABASE_URL", url);
    c
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(c: &mut Command, stdin: Option<&str>) -> Out {
    c.write_stdin(stdin.unwrap_or(""));
    let o = c.output().unwrap();
    Out {
        code: o.status.code().unwrap_or(-1),
        stdout: String::from_utf8(o.stdout).unwrap(),
        stderr: String::from_utf8(o.stderr).unwrap(),
    }
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// Every file under `root` with its bytes.
fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, root, out);
            } else {
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

const LEFT_JOIN: &str = "/* @name orders */\nSELECT c.id, o.total FROM customers c LEFT JOIN orders o ON o.customer_id = c.id WHERE c.id = :id!\n";

#[test]
fn describe_file_writes_nothing() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "q/orders.sql", LEFT_JOIN);
    write(dir.path(), "q/other.sql", "SELECT 1 AS one\n");
    let before = snapshot(dir.path());
    let o = run(
        qf(dir.path(), &db.url).args(["describe", "q/orders.sql"]),
        None,
    );
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stdout.starts_with("orders\n"), "{}", o.stdout);
    assert_eq!(snapshot(dir.path()), before);
}

#[test]
fn describe_sql_flag() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    let o = run(
        qf(dir.path(), &db.url).args([
            "describe",
            "--sql",
            "SELECT id FROM customers WHERE id = :id!",
        ]),
        None,
    );
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stdout.starts_with("adhoc\n"), "{}", o.stdout);
    assert!(o.stdout.contains(":id"), "{}", o.stdout);
    assert!(snapshot(dir.path()).is_empty());
}

#[test]
fn describe_stdin() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    let o = run(
        qf(dir.path(), &db.url).arg("describe"),
        Some("SELECT id FROM customers"),
    );
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stdout.starts_with("adhoc\n"), "{}", o.stdout);
}

#[test]
fn json_is_meta_shape() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "orders.sql", LEFT_JOIN);
    let o = run(
        qf(dir.path(), &db.url).args(["describe", "orders.sql", "--json"]),
        None,
    );
    assert_eq!(o.code, 0, "{}", o.stderr);
    let meta = from_json(&o.stdout).unwrap();
    assert_eq!(meta.name, "orders");
    assert!(meta.generated.is_empty());
    assert_eq!(meta.status, Status::Ok);
}

#[test]
fn text_shows_reasons() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "orders.sql", LEFT_JOIN);
    let o = run(
        qf(dir.path(), &db.url).args(["describe", "orders.sql"]),
        None,
    );
    assert_eq!(o.code, 0, "{}", o.stderr);
    let line = o
        .stdout
        .lines()
        .find(|l| l.trim_start().starts_with("total"))
        .unwrap();
    assert!(line.contains("nullable (outer-join)"), "{}", o.stdout);
}

#[test]
fn broken_sql_exit_1_with_diagnostic() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    let o = run(
        qf(dir.path(), &db.url).args(["describe", "--sql", "SELECT id\nFORM customers"]),
        None,
    );
    assert_eq!(o.code, 1, "{}", o.stderr);
    assert!(
        o.stderr.contains("<sql>:") && o.stderr.contains("error[QF"),
        "{}",
        o.stderr
    );
    assert!(o.stdout.is_empty(), "{}", o.stdout);
}

#[test]
fn two_inputs_usage_error() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.sql", "SELECT 1\n");
    let o = run(
        qf(dir.path(), &db.url).args(["describe", "a.sql", "--sql", "SELECT 1"]),
        None,
    );
    assert_eq!(o.code, 2, "{}", o.stderr);
}
