//! `qf schema` end to end against a real Postgres.
//!
//! Needs `QF_TEST_DATABASE_URL`; fails (never skips) without one. Each test loads qf-pg's fixture
//! schema into its own uniquely named schema (selected through `search_path` in the connection
//! string given to `qf` as `DATABASE_URL`) and drops it afterwards.
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
        "qf_schema_{}_{}_{}",
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

#[test]
fn lists_tables_and_views() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    let o = run(qf(dir.path(), &db.url).arg("schema"), None);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let s = &db.schema;
    let lines: Vec<&str> = o.stdout.lines().collect();
    let mut sorted = lines.clone();
    sorted.sort();
    assert_eq!(lines, sorted, "{}", o.stdout);
    for t in ["addresses", "customers", "drafts", "invoices", "posts"] {
        let line = format!("{s}.{t} (table)");
        assert!(lines.contains(&line.as_str()), "{}", o.stdout);
    }
}

#[test]
fn table_detail_text() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    let o = run(qf(dir.path(), &db.url).args(["schema", "customers"]), None);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let s = &db.schema;
    let out = &o.stdout;
    assert!(
        out.starts_with(&format!("{s}.customers (table)\n")),
        "{out}"
    );
    // Whitespace-insensitive: alignment is covered by the pure rendering tests.
    let flat: Vec<String> = out
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    let has = |l: &str| flat.iter().any(|f| f == l);
    assert!(
        has("id int4 NOT NULL default nextval('customers_id_seq'::regclass)"),
        "{out}"
    );
    assert!(has("email varchar(30) NOT NULL"), "{out}");
    assert!(has("name text null"), "{out}");
    assert!(has("tags text[] null"), "{out}");
    assert!(has("created_at timestamp NOT NULL default now()"), "{out}");
    assert!(has("PK (id)"), "{out}");
    assert!(
        has(&format!("enum {s}.accessLevel: admin | owner | user")),
        "{out}"
    );
}

#[test]
fn foreign_keys_shown() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    let o = run(qf(dir.path(), &db.url).args(["schema", "invoices"]), None);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let s = &db.schema;
    let fk1 = format!("  FK (customer_id) \u{2192} {s}.customers(id)\n");
    let fk2 = format!("  FK (address_id) \u{2192} {s}.addresses(id)\n");
    assert!(o.stdout.contains(&fk1), "{}", o.stdout);
    assert!(o.stdout.contains(&fk2), "{}", o.stdout);
    assert!(o.stdout.contains("numeric(12,2)"), "{}", o.stdout);
}

#[test]
fn json_shape() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    let o = run(
        qf(dir.path(), &db.url).args(["--json", "schema", "customers", "invoices"]),
        None,
    );
    assert_eq!(o.code, 0, "{}", o.stderr);
    let v: serde_json::Value = serde_json::from_str(&o.stdout).expect("one JSON document");
    let tables = v["tables"].as_array().unwrap();
    assert_eq!(tables.len(), 2);
    assert_eq!(tables[0]["name"], "customers");
    assert_eq!(tables[0]["kind"], "table");
    assert_eq!(tables[0]["primaryKey"], serde_json::json!(["id"]));
    assert_eq!(tables[0]["columns"][1]["type"], "varchar(30)");
    assert_eq!(tables[0]["columns"][1]["nullable"], false);
    assert_eq!(tables[0]["columns"][2]["nullable"], true);
    assert!(tables[0]["columns"][0]["default"].is_string());
    // Foreign keys come in constraint-name order: invoices_address_id_fkey, then
    // invoices_customer_id_fkey.
    let fks = tables[1]["foreignKeys"].as_array().unwrap();
    assert_eq!(fks.len(), 2, "{}", o.stdout);
    assert_eq!(fks[0]["columns"], serde_json::json!(["address_id"]));
    assert_eq!(
        fks[0]["references"]["table"],
        format!("{}.addresses", db.schema)
    );
    assert_eq!(fks[1]["columns"], serde_json::json!(["customer_id"]));
    assert_eq!(
        fks[1]["references"]["table"],
        format!("{}.customers", db.schema)
    );
    assert_eq!(
        v["enums"][0]["values"],
        serde_json::json!(["admin", "owner", "user"])
    );
}

#[test]
fn unknown_table_did_you_mean() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    let o = run(qf(dir.path(), &db.url).args(["schema", "custmers"]), None);
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert!(o.stdout.is_empty(), "{}", o.stdout);
    assert!(o.stderr.contains("error[QF091]"), "{}", o.stderr);
    let hint = format!(r#"did you mean "{}.customers"?"#, db.schema);
    assert!(o.stderr.contains(&hint), "{}", o.stderr);
}

#[test]
fn unreachable_database_is_qf090() {
    let dir = tempfile::tempdir().unwrap();
    let o = run(
        qf(dir.path(), "postgres://u:p@127.0.0.1:1/db").arg("schema"),
        None,
    );
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert!(o.stderr.contains("error[QF090]"), "{}", o.stderr);
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
        assert!(!stderr.contains(&pw), "password printed");
    }
    assert!(!stderr.contains("postgres://"), "connection string printed");
}

#[test]
fn names_the_database_on_stderr_without_password() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    let o = run(qf(dir.path(), &db.url).arg("schema"), None);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_names_target(&db.url, &o.stderr);
    assert!(!o.stdout.contains("database:"), "{}", o.stdout);
    let o = run(qf(dir.path(), &db.url).args(["schema", "--json"]), None);
    assert!(
        o.stderr.is_empty() && !o.stdout.contains("database:"),
        "{}",
        o.stderr
    );
}
