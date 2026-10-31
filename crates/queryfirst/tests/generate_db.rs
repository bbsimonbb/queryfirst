//! `qf generate` end to end against a real Postgres.
//!
//! Needs `QF_TEST_DATABASE_URL`; fails (never skips) without one. Each test loads qf-pg's fixture
//! schema into its own uniquely named schema (selected through `search_path` in the connection
//! string given to `qf` as `DATABASE_URL`) and drops it afterwards.
#![cfg(feature = "db-tests")]

use assert_cmd::Command;
use qf_meta::{Status, from_json, sha256_tag};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

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
        "qf_gen_{}_{}_{}",
        std::process::id(),
        nanos,
        N.fetch_add(1, Ordering::Relaxed)
    )
}

fn with_search_path(url: &str, schema: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}options=-c%20search_path%3D{schema}")
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
        Db {
            url: with_search_path(&base, &schema),
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
    c.env("DATABASE_URL", url);
    c
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn generate(project: &Path, url: &str, args: &[&str]) -> Out {
    let o = qf(project, url)
        .arg("generate")
        .args(args)
        .output()
        .unwrap();
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

fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn snapshot(root: &Path) -> BTreeMap<String, (Vec<u8>, SystemTime)> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, (Vec<u8>, SystemTime)>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                let m = std::fs::metadata(&p).unwrap().modified().unwrap();
                out.insert(rel, (std::fs::read(&p).unwrap(), m));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn json(out: &Out) -> serde_json::Value {
    serde_json::from_str(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", out.stdout))
}

const CUSTOMER: &str = "/* @name getCustomer */\nSELECT c.id, c.email, c.name, a.city\nFROM customers c\nLEFT JOIN addresses a ON a.customer_id = c.id\nWHERE c.id = :id\n";

#[test]
fn generates_meta_and_wrapper() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "app/[id]/(queries)/get-customer.psql", CUSTOMER);
    let out = generate(root, &db.url, &[]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let meta = from_json(&read(root, "app/[id]/(queries)/get-customer.qf.json")).unwrap();
    assert_eq!(meta.status, Status::Ok);
    let cols: Vec<_> = meta
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.db_type.name.as_str(), c.nullable))
        .collect();
    assert_eq!(
        cols,
        vec![
            ("id", "int4", false),
            ("email", "varchar", false),
            ("name", "text", true),
            ("city", "text", true)
        ]
    );
    assert_eq!(meta.columns[1].db_type.modifier.as_deref(), Some("(30)"));
    assert!(meta.params[0].required, "{:?}", meta.params[0]);
    let wrapper = read(root, "app/[id]/(queries)/get-customer.qf.ts");
    assert!(wrapper.contains("  id: number;\n"), "{wrapper}");
    assert!(wrapper.contains("  city: string | null;\n"), "{wrapper}");
    assert_eq!(meta.generated[0].hash, sha256_tag(wrapper.as_bytes()));
    assert!(out.stderr.contains("Queryfirst"), "{}", out.stderr);
}

#[test]
fn second_run_writes_nothing() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "app/get-customer.psql", CUSTOMER);
    write(root, "app/invoices.sql", "SELECT id, total FROM invoices");
    assert_eq!(generate(root, &db.url, &[]).code, 0);
    let before = snapshot(root);
    let out = generate(root, &db.url, &["--json"]);
    assert_eq!(out.code, 0);
    assert_eq!(json(&out)["written"], serde_json::json!([]));
    assert_eq!(snapshot(root), before);
}

#[test]
fn crlf_and_lf_sources_identical_outputs() {
    let db = Db::new();
    let lf = tempfile::tempdir().unwrap();
    let crlf = tempfile::tempdir().unwrap();
    let sources = [
        ("app/get-customer.psql", CUSTOMER),
        (
            "app/invoice.psql",
            "/*\n  Invoice totals.\n  @name invoice\n*/\nSELECT i.id,\n  i.total,\n  'é' AS accent\nFROM invoices i\nWHERE i.customer_id = :customerId\n",
        ),
        (
            "app/broken.psql",
            "SELECT id\nFROM invoices\nWHERE nope = 1\n",
        ),
    ];
    for (rel, text) in sources {
        write(lf.path(), rel, text);
        write(crlf.path(), rel, &text.replace('\n', "\r\n"));
    }
    let a = generate(lf.path(), &db.url, &["--json"]);
    let b = generate(crlf.path(), &db.url, &["--json"]);
    assert_eq!(a.code, 1);
    assert_eq!(a.stdout, b.stdout);
    let written = json(&a)["written"].as_array().unwrap().clone();
    assert_eq!(written.len(), 6);
    for rel in written {
        let rel = rel.as_str().unwrap();
        assert_eq!(
            std::fs::read(lf.path().join(rel)).unwrap(),
            std::fs::read(crlf.path().join(rel)).unwrap(),
            "{rel}"
        );
    }
}

#[test]
fn broken_query_three_channels() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let rel = "app/get-invoice.psql";
    let good = "/* @name getInvoice */\nSELECT id, total\nFROM invoices\nWHERE id = :id\n";
    write(root, rel, good);
    assert_eq!(generate(root, &db.url, &[]).code, 0);
    let good_meta = read(root, "app/get-invoice.qf.json");
    let good_wrapper = read(root, "app/get-invoice.qf.ts");

    write(
        root,
        rel,
        "/* @name getInvoice */\nSELECT id, vat_rate\nFROM invoices\nWHERE id = :id\n",
    );
    let out = generate(root, &db.url, &[]);
    // 1. Command output.
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr.contains(
            "app/get-invoice.psql:2:12: error[QF010]: column \"vat_rate\" does not exist on \"invoices\"\n"
        ),
        "{}",
        out.stderr
    );
    // 2. Meta.
    let meta = from_json(&read(root, "app/get-invoice.qf.json")).unwrap();
    assert!(matches!(meta.status, Status::Broken { .. }));
    let old = from_json(&good_meta).unwrap();
    assert_eq!(meta.params, old.params);
    assert_eq!(meta.columns, old.columns);
    // 3. Wrapper: previous types plus the error.
    let wrapper = read(root, "app/get-invoice.qf.ts");
    assert!(wrapper.starts_with(good_wrapper.trim_end()), "{wrapper}");
    assert!(
        wrapper.contains("export const queryfirstError: never = \"getInvoice: column \\\"vat_rate\\\" does not exist"),
        "{wrapper}"
    );

    write(root, rel, good);
    let out = generate(root, &db.url, &[]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(!out.stderr.contains("QF010"));
    assert_eq!(read(root, "app/get-invoice.qf.json"), good_meta);
    assert_eq!(read(root, "app/get-invoice.qf.ts"), good_wrapper);
}

/// A TCP proxy to the test database that drops every connection once a client sends `marker`.
struct Proxy {
    port: u16,
    dropped: Arc<AtomicBool>,
}

fn upstream_addr(url: &str) -> String {
    let after_scheme = url.split_once("://").map_or(url, |(_, r)| r);
    let after_user = after_scheme
        .rsplit_once('@')
        .map_or(after_scheme, |(_, r)| r);
    let host = after_user.split(['/', '?']).next().unwrap();
    if host.contains(':') {
        host.to_string()
    } else {
        format!("{host}:5432")
    }
}

fn start_proxy(upstream: String, marker: &'static [u8]) -> Proxy {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let dropped = Arc::new(AtomicBool::new(false));
    let streams: Arc<Mutex<Vec<TcpStream>>> = Arc::default();
    let flag = dropped.clone();
    std::thread::spawn(move || {
        for client in listener.incoming() {
            let Ok(client) = client else { continue };
            if flag.load(Ordering::SeqCst) {
                let _ = client.shutdown(Shutdown::Both);
                continue;
            }
            let server = TcpStream::connect(&upstream).unwrap();
            streams.lock().unwrap().push(client.try_clone().unwrap());
            streams.lock().unwrap().push(server.try_clone().unwrap());
            let pump = |mut from: TcpStream, mut to: TcpStream, watch: bool| {
                let (flag, streams) = (flag.clone(), streams.clone());
                std::thread::spawn(move || {
                    let mut buf = [0u8; 16384];
                    let mut seen: Vec<u8> = Vec::new();
                    loop {
                        let n = match from.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        if watch {
                            seen.extend_from_slice(&buf[..n]);
                            if seen.windows(marker.len()).any(|w| w == marker) {
                                flag.store(true, Ordering::SeqCst);
                                for s in streams.lock().unwrap().iter() {
                                    let _ = s.shutdown(Shutdown::Both);
                                }
                                break;
                            }
                            if seen.len() > 1 << 20 {
                                seen.drain(..seen.len() - marker.len());
                            }
                        }
                        if to.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                    let _ = to.shutdown(Shutdown::Write);
                });
            };
            pump(
                client.try_clone().unwrap(),
                server.try_clone().unwrap(),
                true,
            );
            pump(server, client, false);
        }
    });
    Proxy { port, dropped }
}

#[test]
fn connection_lost_writes_nothing() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "app/a.psql", CUSTOMER);
    assert_eq!(generate(root, &db.url, &[]).code, 0);
    write(
        root,
        "app/a.psql",
        "SELECT id, email FROM customers WHERE id = :other",
    );
    write(
        root,
        "app/b.psql",
        "SELECT id AS qf_drop_marker FROM customers",
    );
    write(root, "app/c.psql", "SELECT id FROM invoices");
    let before = snapshot(root);

    let proxy = start_proxy(upstream_addr(&db.url), b"qf_drop_marker");
    let addr = upstream_addr(&db.url);
    let url = db
        .url
        .replacen(&addr, &format!("127.0.0.1:{}", proxy.port), 1);
    let out = generate(root, &url, &["--json"]);
    assert!(
        proxy.dropped.load(Ordering::SeqCst),
        "the proxy never saw the marker"
    );
    assert_eq!(out.code, 2, "{}", out.stdout);
    let v = json(&out);
    assert!(
        v["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "QF090"),
        "{}",
        out.stdout
    );
    assert_eq!(v["written"], serde_json::json!([]));
    assert_eq!(snapshot(root), before);
    assert!(!out.stdout.contains("qftest"), "connection string leaked");
}

#[test]
fn orphan_cleanup_full_run_only() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "app/a.psql", CUSTOMER);
    write(root, "app/b.psql", "SELECT id FROM invoices");
    write(root, "app/hand-written.qf.ts", "export const x = 1;\n");
    assert_eq!(generate(root, &db.url, &[]).code, 0);
    std::fs::remove_file(root.join("app/b.psql")).unwrap();

    let out = generate(root, &db.url, &["--json", "app/a.psql"]);
    assert_eq!(json(&out)["deleted"], serde_json::json!([]));
    assert!(root.join("app/b.qf.ts").exists());

    let out = generate(root, &db.url, &["--json"]);
    assert_eq!(
        json(&out)["deleted"],
        serde_json::json!(["app/b.qf.json", "app/b.qf.ts"])
    );
    assert!(root.join("app/hand-written.qf.ts").exists());
    assert!(root.join("app/a.qf.ts").exists());
}

#[test]
fn duplicate_column_qf012() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "q.psql",
        "SELECT c.id, a.id FROM customers c JOIN addresses a ON a.customer_id = c.id",
    );
    let out = generate(dir.path(), &db.url, &[]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("error[QF012]"), "{}", out.stderr);
}

#[test]
fn anonymous_column_qf013() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "q.psql", "SELECT id + 1 FROM customers");
    let out = generate(dir.path(), &db.url, &[]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("error[QF013]"), "{}", out.stderr);
}

#[test]
fn unknown_type_override_target_qf004() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "q.psql",
        "/* @name q\n   @type emial -> Email from \"@/types\" */\nSELECT id, email FROM customers",
    );
    let out = generate(dir.path(), &db.url, &[]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("q.psql:2:4: error[QF004]"),
        "{}",
        out.stderr
    );
}

#[test]
fn type_override_on_param_and_column() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "q.psql",
        "/* @name q\n   @type access -> Access from \"@/types/access\"\n   @type :prefs -> Prefs from \"@/types/prefs\" */\nUPDATE customers SET prefs = :prefs WHERE id = :id! RETURNING access",
    );
    let out = generate(dir.path(), &db.url, &[]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let meta = from_json(&read(dir.path(), "q.qf.json")).unwrap();
    assert_eq!(
        meta.columns[0].type_override.as_ref().unwrap().name,
        "Access"
    );
    let p = meta.params.iter().find(|p| p.name == "prefs").unwrap();
    assert_eq!(p.type_override.as_ref().unwrap().name, "Prefs");
    let wrapper = read(dir.path(), "q.qf.ts");
    assert!(
        wrapper.contains("import type { Access } from '@/types/access';"),
        "{wrapper}"
    );
    assert!(
        wrapper.contains("import type { Prefs } from '@/types/prefs';"),
        "{wrapper}"
    );
    assert!(wrapper.contains("  access: Access;\n"), "{wrapper}");
}

#[test]
fn json_output_shape() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.psql", "SELECT nope FROM customers");
    let out = generate(dir.path(), &db.url, &["--json"]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.is_empty(), "{}", out.stderr);
    let v = json(&out);
    for k in [
        "command",
        "ok",
        "summary",
        "diagnostics",
        "written",
        "deleted",
    ] {
        assert!(v.get(k).is_some(), "missing {k}");
    }
    assert_eq!(v["command"], "generate");
    assert_eq!(v["ok"], false);
    for k in ["queries", "errors", "warnings", "written", "deleted"] {
        assert!(v["summary"].get(k).is_some(), "missing summary.{k}");
    }
    let d = &v["diagnostics"][0];
    for k in ["file", "line", "col", "code", "severity", "message", "hint"] {
        assert!(d.get(k).is_some(), "missing diagnostics[].{k}");
    }
    assert_eq!(d["file"], "a.psql");
    assert_eq!(d["code"], "QF010");
    assert_eq!(d["line"], 1);
    assert_eq!(d["col"], 8);
}

#[test]
fn single_file_scope_fast() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for i in 0..30 {
        write(root, &format!("app/q{i}.psql"), "SELECT id FROM customers");
    }
    write(root, "app/target.psql", CUSTOMER);
    let start = Instant::now();
    let out = generate(root, &db.url, &["app/target.psql"]);
    let took = start.elapsed();
    println!("single-file qf generate took {took:?}");
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(!root.join("app/q0.qf.json").exists());
    assert!(took < Duration::from_secs(2), "took {took:?}");
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
fn summary_names_the_database_without_password() {
    let db = Db::new();
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.psql", CUSTOMER);
    let out = generate(dir.path(), &db.url, &[]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_names_target(&db.url, &out.stderr);
    // Not in the JSON output.
    let out = generate(dir.path(), &db.url, &["--json"]);
    assert!(!out.stdout.contains("database:"), "{}", out.stdout);
}
