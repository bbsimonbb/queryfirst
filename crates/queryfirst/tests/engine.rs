//! Engine tests without a database: describe is faked, the catalog is empty.

use qf_meta::{Status, Transform, TypeCategory, TypeRef, from_json, sha256_tag};
use qf_pg::{Catalog, Described, DescribedColumn, PgQueryError};
use queryfirst::cmd::generate::generate_with;
use queryfirst::config::{Config, parse_config};
use queryfirst::discover::discover;
use queryfirst::engine::Describe;
use queryfirst::report::{Report, to_json};
use queryfirst::writer::write_if_changed;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

fn ty(name: &str) -> TypeRef {
    TypeRef {
        schema: "pg_catalog".into(),
        name: name.into(),
        array: false,
        modifier: None,
        category: TypeCategory::Base,
        enum_values: None,
    }
}

/// Number of `$n` placeholders (the highest n).
fn placeholders(sql: &str) -> usize {
    let b = sql.as_bytes();
    let mut max = 0;
    for i in 0..b.len() {
        if b[i] == b'$' {
            let digits: String = sql[i + 1..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(n) = digits.parse::<usize>() {
                max = max.max(n);
            }
        }
    }
    max
}

/// Fake describe: every parameter is `int4`; columns come from `columns_for`; statements
/// containing `boom` lose the connection, `FORM` is a syntax error.
struct Fake;

fn columns_for(sql: &str) -> Vec<(&'static str, &'static str)> {
    if sql.starts_with("INSERT") {
        vec![]
    } else if sql.contains("dup") {
        vec![("id", "int4"), ("id", "int4")]
    } else if sql.contains("anon") {
        vec![("?column?", "int4")]
    } else if sql.contains("total") {
        vec![("total!", "numeric")]
    } else {
        vec![("id", "int4"), ("email", "varchar")]
    }
}

impl Describe for Fake {
    async fn describe(&self, sql: &str) -> Result<Described, PgQueryError> {
        if sql.contains("boom") {
            return Err(PgQueryError::Connection("connection reset by peer".into()));
        }
        if let Some(at) = sql.find("FORM") {
            return Err(PgQueryError::Db {
                code: "42601".into(),
                message: "syntax error at or near \"FORM\"".into(),
                detail: None,
                hint: None,
                position: Some(sql[..at].chars().count() as u32 + 1),
            });
        }
        Ok(Described {
            params: (0..placeholders(sql)).map(|_| ty("int4")).collect(),
            columns: columns_for(sql)
                .into_iter()
                .map(|(n, t)| DescribedColumn {
                    name: n.into(),
                    db_type: ty(t),
                    source: None,
                })
                .collect(),
        })
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

async fn run(root: &Path, cfg: &Config, paths: &[PathBuf]) -> Report {
    let files = discover(root, cfg, paths).unwrap();
    generate_with(
        root,
        cfg,
        &files,
        paths.is_empty(),
        &Fake,
        &Catalog::default(),
    )
    .await
}

/// Every file under `root`: relative path -> (bytes, mtime).
fn snapshot(root: &Path) -> BTreeMap<String, (Vec<u8>, SystemTime)> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, (Vec<u8>, SystemTime)>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let p = e.path();
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

const GET: &str = "/* @name getCustomer */\nSELECT id, email FROM customers WHERE id = :id\n";

#[tokio::test]
async fn generates_meta_and_wrapper() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "app/[id]/(queries)/get-customer.psql", GET);
    let r = run(root, &Config::default(), &[]).await;
    assert_eq!(r.exit_code(), 0, "{:?}", r.diagnostics);
    assert_eq!(
        r.written,
        vec![
            "app/[id]/(queries)/get-customer.qf.json",
            "app/[id]/(queries)/get-customer.query.ts"
        ]
    );
    let meta = from_json(&read(root, "app/[id]/(queries)/get-customer.qf.json")).unwrap();
    assert_eq!(meta.source, "app/[id]/(queries)/get-customer.psql");
    assert_eq!(meta.name, "getCustomer");
    assert_eq!(meta.status, Status::Ok);
    assert_eq!(meta.sql_hash, sha256_tag(GET.as_bytes()));
    assert_eq!(
        meta.statement.db,
        "SELECT id, email FROM customers WHERE id = $1"
    );
    let wrapper = read(root, "app/[id]/(queries)/get-customer.query.ts");
    assert!(wrapper.contains("export interface IGetCustomerResult"));
    assert_eq!(meta.generated.len(), 1);
    assert_eq!(
        meta.generated[0].path,
        "app/[id]/(queries)/get-customer.query.ts"
    );
    assert_eq!(meta.generated[0].hash, sha256_tag(wrapper.as_bytes()));
    // The empty catalog means the analyzer gives up: a note, not an error.
    assert!(r.diagnostics.iter().any(|d| d.code == "QF020"));
    assert_eq!(r.summary.errors, 0);
}

#[tokio::test]
async fn second_run_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "app/get-customer.psql", GET);
    write(root, "app/b.sql", "SELECT id, email FROM customers");
    assert_eq!(run(root, &Config::default(), &[]).await.written.len(), 4);
    let before = snapshot(root);
    let r = run(root, &Config::default(), &[]).await;
    assert!(r.written.is_empty(), "{:?}", r.written);
    assert!(r.deleted.is_empty());
    assert_eq!(snapshot(root), before);
}

#[tokio::test]
async fn crlf_and_lf_sources_identical_outputs() {
    let lf = tempfile::tempdir().unwrap();
    let crlf = tempfile::tempdir().unwrap();
    let sources = [
        ("app/get-customer.psql", GET.to_string()),
        (
            "app/multi.psql",
            "/*\n  Customers by id.\n  @name multi\n  @type email -> Email from \"@/types\"\n*/\nSELECT id,\n  email\nFROM customers\nWHERE id = :id!\n  AND x = 'é'\n".to_string(),
        ),
        (
            "app/broken.psql",
            "/* @name broken */\nSELECT id\nFORM customers\n".to_string(),
        ),
    ];
    for (rel, text) in &sources {
        write(lf.path(), rel, text);
        write(crlf.path(), rel, &text.replace('\n', "\r\n"));
    }
    let a = run(lf.path(), &Config::default(), &[]).await;
    let b = run(crlf.path(), &Config::default(), &[]).await;
    assert_eq!(a.written, b.written);
    assert_eq!(a.exit_code(), 1);
    assert_eq!(
        a.diagnostics
            .iter()
            .map(|d| (d.code.clone(), d.line, d.col))
            .collect::<Vec<_>>(),
        b.diagnostics
            .iter()
            .map(|d| (d.code.clone(), d.line, d.col))
            .collect::<Vec<_>>()
    );
    for rel in &a.written {
        let x = read(lf.path(), rel);
        assert_eq!(x, read(crlf.path(), rel), "{rel}");
        assert!(!x.contains('\r'), "{rel}");
        assert!(x.ends_with('\n'), "{rel}");
    }
}

#[tokio::test]
async fn broken_query_three_channels() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let rel = "app/get-customer.psql";
    write(root, rel, GET);
    assert_eq!(run(root, &Config::default(), &[]).await.exit_code(), 0);
    let good_meta = from_json(&read(root, "app/get-customer.qf.json")).unwrap();

    write(
        root,
        rel,
        "/* @name getCustomer */\nSELECT id, email\nFORM customers WHERE id = :id\n",
    );
    let r = run(root, &Config::default(), &[]).await;
    // 1. Command output.
    assert_eq!(r.exit_code(), 1);
    let d = r.diagnostics.iter().find(|d| d.code == "QF010").unwrap();
    assert_eq!(d.file.as_deref(), Some(rel));
    assert_eq!((d.line, d.col), (3, 1));
    let human = queryfirst::report::render_human(&r);
    assert!(
        human.contains("app/get-customer.psql:3:1: error[QF010]: syntax error at or near \"FORM\""),
        "{human}"
    );
    // 2. Meta: broken, last good params/columns kept.
    let meta = from_json(&read(root, "app/get-customer.qf.json")).unwrap();
    let Status::Broken { diagnostics } = &meta.status else {
        panic!("not broken: {:?}", meta.status)
    };
    assert_eq!(diagnostics[0].code, "QF010");
    assert_eq!(meta.params, good_meta.params);
    assert_eq!(meta.columns, good_meta.columns);
    assert_ne!(meta.sql_hash, good_meta.sql_hash);
    // 3. Wrapper: previous types plus one deliberate error.
    let wrapper = read(root, "app/get-customer.query.ts");
    assert!(wrapper.contains("export interface IGetCustomerResult"));
    assert!(wrapper.contains("  email: string | null;"), "{wrapper}");
    assert!(wrapper.contains(
        "export const queryfirstError: never = \"getCustomer: syntax error at or near \\\"FORM\\\"\";"
    ));
    assert_eq!(meta.generated[0].hash, sha256_tag(wrapper.as_bytes()));

    // Fixing the query clears all three.
    write(root, rel, GET);
    let r = run(root, &Config::default(), &[]).await;
    assert_eq!(r.exit_code(), 0);
    assert!(!r.diagnostics.iter().any(|d| d.code == "QF010"));
    assert_eq!(
        from_json(&read(root, "app/get-customer.qf.json")).unwrap(),
        good_meta
    );
    assert!(!read(root, "app/get-customer.query.ts").contains("queryfirstError"));
}

#[tokio::test]
async fn broken_without_previous_is_error_only() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "a.psql", "SELECT id FORM customers");
    let r = run(root, &Config::default(), &[]).await;
    assert_eq!(r.exit_code(), 1);
    let wrapper = read(root, "a.query.ts");
    assert!(!wrapper.contains("interface"));
    assert!(wrapper.contains("export const queryfirstError: never = \"a: syntax error"));
    let meta = from_json(&read(root, "a.qf.json")).unwrap();
    assert!(meta.params.is_empty() && meta.columns.is_empty());
}

#[tokio::test]
async fn parse_error_is_broken_query() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "two.psql", "SELECT 1;\nSELECT 2;\n");
    let r = run(root, &Config::default(), &[]).await;
    assert_eq!(r.exit_code(), 1);
    let d = r.diagnostics.iter().find(|d| d.code == "QF002").unwrap();
    assert_eq!(d.line, 2);
    let meta = from_json(&read(root, "two.qf.json")).unwrap();
    assert!(matches!(meta.status, Status::Broken { .. }));
    assert!(read(root, "two.query.ts").contains("queryfirstError"));
}

#[tokio::test]
async fn connection_lost_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "app/a.psql", GET);
    assert_eq!(run(root, &Config::default(), &[]).await.exit_code(), 0);
    write(
        root,
        "app/a.psql",
        "SELECT id, email FROM customers WHERE id = :changed",
    );
    write(root, "app/b.psql", "SELECT boom FROM customers");
    write(root, "app/c.psql", "SELECT id FROM customers");
    let before = snapshot(root);
    let r = run(root, &Config::default(), &[]).await;
    assert_eq!(r.exit_code(), 2);
    assert!(r.diagnostics.iter().any(|d| d.code == "QF090"));
    assert!(r.written.is_empty());
    assert_eq!(snapshot(root), before);
}

#[tokio::test]
async fn orphan_cleanup_full_run_only() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "app/a.psql", GET);
    write(root, "app/b.psql", "SELECT id FROM customers");
    write(root, "app/stray.query.ts", "// mine\n");
    write(root, "app/junk.qf.json", "not json\n");
    assert_eq!(run(root, &Config::default(), &[]).await.exit_code(), 0);
    std::fs::remove_file(root.join("app/b.psql")).unwrap();

    // A scoped run never deletes.
    let r = run(root, &Config::default(), &[root.join("app/a.psql")]).await;
    assert!(r.deleted.is_empty());
    assert!(root.join("app/b.qf.json").exists());
    assert!(root.join("app/b.query.ts").exists());

    let r = run(root, &Config::default(), &[]).await;
    assert_eq!(r.deleted, vec!["app/b.qf.json", "app/b.query.ts"]);
    assert!(!root.join("app/b.qf.json").exists());
    assert!(!root.join("app/b.query.ts").exists());
    assert!(root.join("app/a.query.ts").exists());
    assert!(root.join("app/stray.query.ts").exists());
    assert!(root.join("app/junk.qf.json").exists());
}

#[tokio::test]
async fn orphan_cleanup_ignores_unlisted_and_unsafe_paths() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "app/b.psql", "SELECT id FROM customers");
    assert_eq!(run(root, &Config::default(), &[]).await.exit_code(), 0);
    // Tamper: the meta lists a file outside the project and a file it did not generate.
    let mut meta = from_json(&read(root, "app/b.qf.json")).unwrap();
    meta.generated.push(qf_meta::GeneratedFile {
        path: "../outside.txt".into(),
        hash: "sha256:00".into(),
    });
    std::fs::write(
        root.join("app/b.qf.json"),
        qf_meta::to_canonical_json(&meta),
    )
    .unwrap();
    write(root, "app/other.ts", "// not listed\n");
    std::fs::remove_file(root.join("app/b.psql")).unwrap();
    let r = run(root, &Config::default(), &[]).await;
    assert_eq!(r.deleted, vec!["app/b.qf.json", "app/b.query.ts"]);
    assert!(root.join("app/other.ts").exists());
}

#[tokio::test]
async fn template_change_removes_previous_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "app/a.psql", GET);
    assert_eq!(run(root, &Config::default(), &[]).await.exit_code(), 0);
    let cfg = parse_config(
        r#"{ "generators": [ { "name": "typescript", "emitTemplate": "{{dir}}/{{name}}.gen.ts" } ] }"#,
    )
    .unwrap();
    let r = run(root, &cfg, &[root.join("app/a.psql")]).await;
    assert_eq!(r.deleted, vec!["app/a.query.ts"]);
    assert!(root.join("app/a.gen.ts").exists());
}

#[tokio::test]
async fn duplicate_column_qf012() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "q.psql", "SELECT dup.id, dup.id FROM dup");
    let r = run(dir.path(), &Config::default(), &[]).await;
    assert_eq!(r.exit_code(), 1);
    let d = r.diagnostics.iter().find(|d| d.code == "QF012").unwrap();
    assert!(d.message.contains("\"id\""), "{}", d.message);
}

#[tokio::test]
async fn anonymous_column_qf013() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "q.psql", "SELECT 1 + anon FROM t");
    let r = run(dir.path(), &Config::default(), &[]).await;
    assert_eq!(r.exit_code(), 1);
    assert!(r.diagnostics.iter().any(|d| d.code == "QF013"));
}

#[tokio::test]
async fn alias_override_strips_suffix() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "q.psql", "SELECT sum(x) AS \"total!\" FROM t");
    let r = run(dir.path(), &Config::default(), &[]).await;
    assert_eq!(r.exit_code(), 0);
    let meta = from_json(&read(dir.path(), "q.qf.json")).unwrap();
    assert_eq!(meta.columns[0].name, "total");
    assert!(!meta.columns[0].nullable);
    assert_eq!(
        meta.columns[0].nullable_reason,
        qf_meta::NullableReason::Override
    );
}

#[tokio::test]
async fn unknown_type_override_target_qf004() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "q.psql",
        "/* @name q\n   @type nope -> Nope from \"@/types\"\n   @type :missing -> X */\nSELECT id, email FROM customers",
    );
    let r = run(dir.path(), &Config::default(), &[]).await;
    assert_eq!(r.exit_code(), 1);
    let qf004: Vec<_> = r.diagnostics.iter().filter(|d| d.code == "QF004").collect();
    assert_eq!(qf004.len(), 2, "{:?}", r.diagnostics);
    assert_eq!((qf004[0].line, qf004[0].col), (2, 4));
    assert!(qf004[0].message.contains("nope"));
    assert_eq!(qf004[1].line, 3);
    assert!(qf004[1].message.contains(":missing"));
}

#[tokio::test]
async fn type_override_on_param_and_column() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "q.psql",
        "/* @name q\n   @type email -> Email from \"@/types\"\n   @type :id -> CustomerId[] from \"@/ids\" */\nSELECT id, email FROM customers WHERE id = :id",
    );
    let r = run(dir.path(), &Config::default(), &[]).await;
    assert_eq!(r.exit_code(), 0, "{:?}", r.diagnostics);
    let meta = from_json(&read(dir.path(), "q.qf.json")).unwrap();
    let col = meta.columns.iter().find(|c| c.name == "email").unwrap();
    let o = col.type_override.as_ref().unwrap();
    assert_eq!(
        (o.name.as_str(), o.module.as_deref(), o.array),
        ("Email", Some("@/types"), false)
    );
    let p = &meta.params[0];
    let o = p.type_override.as_ref().unwrap();
    assert_eq!(
        (o.name.as_str(), o.module.as_deref(), o.array),
        ("CustomerId", Some("@/ids"), true)
    );
    let wrapper = read(dir.path(), "q.query.ts");
    assert!(
        wrapper.contains("import type { Email } from '@/types';"),
        "{wrapper}"
    );
    assert!(
        wrapper.contains("import type { CustomerId } from '@/ids';"),
        "{wrapper}"
    );
}

#[tokio::test]
async fn params_in_pgtyped_order_with_pick_key_bang() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "q.psql",
        "/* @name q\n   @param ids -> (...)\n   @param c -> (email!, name) */\nSELECT id, email FROM t WHERE b = :b AND a = :a! AND id IN :ids AND (email, name) = :c",
    );
    let r = run(dir.path(), &Config::default(), &[]).await;
    assert_eq!(r.exit_code(), 0, "{:?}", r.diagnostics);
    let meta = from_json(&read(dir.path(), "q.qf.json")).unwrap();
    let names: Vec<_> = meta.params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["ids", "c", "b", "a"]);
    assert_eq!(
        meta.statement.db,
        "SELECT id, email FROM t WHERE b = $4 AND a = $5 AND id IN ($1) AND (email, name) = ($2,$3)"
    );
    assert_eq!(meta.params[0].db_index, vec![1]);
    assert_eq!(meta.params[0].transform, Transform::Spread);
    let Transform::Pick { keys } = &meta.params[1].transform else {
        panic!("{:?}", meta.params[1].transform)
    };
    assert_eq!(keys[0].name, "email");
    assert!(keys[0].required);
    assert_eq!(keys[0].db_index, 2);
    assert!(!keys[1].required);
    assert_eq!(keys[1].db_index, 3);
    assert_eq!(meta.params[2].db_index, vec![4]);
    assert!(!meta.params[2].required);
    assert!(meta.params[3].required);
    assert_eq!(
        meta.params[3].required_reason,
        qf_meta::RequiredReason::Annotation
    );
}

#[tokio::test]
async fn output_conflict_is_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "q.psql", "SELECT id FROM t");
    write(dir.path(), "q.sql", "SELECT id FROM t");
    let r = run(dir.path(), &Config::default(), &[]).await;
    assert_eq!(r.exit_code(), 2);
    assert!(r.written.is_empty());
    assert!(!dir.path().join("q.qf.json").exists());
}

#[tokio::test]
async fn json_output_shape() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.psql", "SELECT id FORM t");
    let r = run(dir.path(), &Config::default(), &[]).await;
    let v = to_json(&r);
    let keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        vec![
            "command",
            "ok",
            "summary",
            "diagnostics",
            "written",
            "deleted"
        ]
    );
    assert_eq!(v["command"], "generate");
    assert_eq!(v["ok"], false);
    let s: Vec<_> = v["summary"].as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        s,
        vec!["queries", "errors", "warnings", "written", "deleted"]
    );
    assert_eq!(v["summary"]["queries"], 1);
    assert_eq!(v["summary"]["errors"], 1);
    assert_eq!(v["summary"]["written"], 2);
    let d = &v["diagnostics"][0];
    let dk: Vec<_> = d.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        dk,
        vec!["file", "line", "col", "code", "severity", "message", "hint"]
    );
    assert_eq!(d["file"], "a.psql");
    assert_eq!(d["severity"], "error");
    assert_eq!(v["written"][0], "a.qf.json");
}

#[test]
fn write_if_changed_only_writes_changes() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a/b/c.txt");
    assert!(write_if_changed(&p, "x\r\ny").unwrap());
    assert_eq!(std::fs::read(&p).unwrap(), b"x\ny\n");
    let m = std::fs::metadata(&p).unwrap().modified().unwrap();
    assert!(!write_if_changed(&p, "x\ny\n").unwrap());
    assert_eq!(std::fs::metadata(&p).unwrap().modified().unwrap(), m);
    assert!(write_if_changed(&p, "z\n").unwrap());
    assert_eq!(std::fs::read(&p).unwrap(), b"z\n");
}
