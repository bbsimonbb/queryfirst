//! `qf check` without a database: projects are built from a source, a meta and the wrapper
//! produced by the generator.

use assert_cmd::Command;
use qf_gen_ts::{PgtypedBinding, TsOptions, generate};
use qf_meta::{
    GeneratedFile, QueryMeta, Status, from_json, meta_path_for, sha256_tag, to_canonical_json,
};
use std::path::Path;
use std::time::{Duration, Instant};

const SQL: &str = "/* @name addressBelongsToAccount */\nSELECT 1;\n";

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap()
}

/// Write `source` with `sql`, its meta (from the committed fixture) and its wrapper.
fn add_query(root: &Path, source: &str, sql: &str, broken: bool) {
    let mut meta: QueryMeta =
        from_json(include_str!("../../qf-meta/tests/fixtures/address.qf.json")).unwrap();
    meta.source = source.into();
    meta.sql_hash = sha256_tag(sql.as_bytes());
    if broken {
        meta.status = Status::Broken {
            diagnostics: vec![qf_meta::MetaDiagnostic {
                code: "QF020".into(),
                severity: qf_meta::Severity::Error,
                message: "syntax error".into(),
                hint: None,
                line: 2,
                col: 1,
            }],
        };
    }
    let out = generate(&meta, None, &TsOptions::default(), &PgtypedBinding).unwrap();
    meta.generated = vec![GeneratedFile {
        path: out.path.clone(),
        hash: sha256_tag(out.content.as_bytes()),
    }];
    write(root, source, sql);
    write(root, &qf_gen_ts_meta_rel(source), &to_canonical_json(&meta));
    write(root, &out.path, &out.content);
}

fn qf_gen_ts_meta_rel(source: &str) -> String {
    meta_path_for(Path::new(source))
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

fn wrapper(root: &Path, source: &str) -> String {
    let m = from_json(&read(root, &qf_gen_ts_meta_rel(source))).unwrap();
    m.generated[0].path.clone()
}

fn check(root: &Path, args: &[&str]) -> (i32, serde_json::Value) {
    let out = Command::cargo_bin("qf")
        .unwrap()
        .current_dir(root)
        .env_remove("DATABASE_URL")
        .arg("check")
        .arg("--json")
        .args(args)
        .output()
        .unwrap();
    let v = serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null);
    (out.status.code().unwrap(), v)
}

fn codes(v: &serde_json::Value) -> Vec<(String, String)> {
    v["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["code"].as_str().unwrap().to_string(),
                d["file"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    add_query(dir.path(), "q/address.psql", SQL, false);
    dir
}

fn expect(root: &Path, args: &[&str], code: &str, file: &str) {
    let (status, v) = check(root, args);
    assert_eq!(status, 1, "{v}");
    assert_eq!(codes(&v), vec![(code.to_string(), file.to_string())], "{v}");
}

#[test]
fn clean_project_passes() {
    let dir = project();
    let (status, v) = check(dir.path(), &[]);
    assert_eq!(status, 0, "{v}");
    assert_eq!(v["command"], "check");
    assert_eq!(v["summary"]["queries"], 1);
    assert!(codes(&v).is_empty());
}

#[test]
fn crlf_checkout_passes() {
    let dir = project();
    let w = wrapper(dir.path(), "q/address.psql");
    for f in ["q/address.psql", w.as_str()] {
        let t = read(dir.path(), f).replace('\n', "\r\n");
        std::fs::write(dir.path().join(f), t).unwrap();
    }
    let (status, v) = check(dir.path(), &[]);
    assert_eq!(status, 0, "{v}");
}

#[test]
fn source_without_meta_is_qf034() {
    let dir = project();
    write(dir.path(), "q/other.psql", "SELECT 2;\n");
    expect(dir.path(), &[], "QF034", "q/other.psql");
}

#[test]
fn changed_sql_is_qf030() {
    let dir = project();
    write(dir.path(), "q/address.psql", "SELECT 2;\n");
    expect(dir.path(), &[], "QF030", "q/address.psql");
}

#[test]
fn broken_meta_is_qf033_only() {
    let dir = tempfile::tempdir().unwrap();
    add_query(dir.path(), "q/address.psql", SQL, true);
    expect(dir.path(), &[], "QF033", "q/address.psql");
}

#[test]
fn edited_wrapper_is_qf031() {
    let dir = project();
    let w = wrapper(dir.path(), "q/address.psql");
    let t = read(dir.path(), &w) + "// hand edit\n";
    write(dir.path(), &w, &t);
    expect(dir.path(), &[], "QF031", &w);
}

#[test]
fn missing_wrapper_is_qf031() {
    let dir = project();
    let w = wrapper(dir.path(), "q/address.psql");
    std::fs::remove_file(dir.path().join(&w)).unwrap();
    expect(dir.path(), &[], "QF031", &w);
}

#[test]
fn regeneration_mismatch_is_qf032() {
    // The wrapper is unedited (hash == recorded) but was made by something else.
    let dir = project();
    let w = wrapper(dir.path(), "q/address.psql");
    let old = "// made by another version\n";
    write(dir.path(), &w, old);
    let mp = qf_gen_ts_meta_rel("q/address.psql");
    let mut m = from_json(&read(dir.path(), &mp)).unwrap();
    m.generated[0].hash = sha256_tag(old.as_bytes());
    write(dir.path(), &mp, &to_canonical_json(&m));
    expect(dir.path(), &[], "QF032", &w);
}

#[test]
fn orphan_meta_is_qf035() {
    let dir = project();
    std::fs::remove_file(dir.path().join("q/address.psql")).unwrap();
    expect(dir.path(), &[], "QF035", "q/address.qf.json");
}

#[test]
fn orphan_scan_is_limited_to_path_arguments() {
    let dir = project();
    add_query(dir.path(), "r/other.psql", SQL, false);
    std::fs::remove_file(dir.path().join("r/other.psql")).unwrap();
    let (status, v) = check(dir.path(), &["q"]);
    assert_eq!(status, 0, "{v}");
    expect(dir.path(), &["r"], "QF035", "r/other.qf.json");
    expect(dir.path(), &[], "QF035", "r/other.qf.json");
}

#[test]
fn literal_bracket_dirs_work() {
    let dir = tempfile::tempdir().unwrap();
    add_query(dir.path(), "app/[id]/(queries)/a.psql", SQL, false);
    let (status, v) = check(dir.path(), &["app/[id]/(queries)"]);
    assert_eq!(status, 0, "{v}");
    assert_eq!(v["summary"]["queries"], 1);
}

#[test]
fn bad_config_exits_2() {
    let dir = project();
    write(dir.path(), "queryfirst.json", "{ \"incldue\": [] }");
    let (status, _) = check(dir.path(), &[]);
    assert_eq!(status, 2);
}

#[test]
fn no_db_env_needed() {
    let dir = project();
    let start = Instant::now();
    let out = Command::cargo_bin("qf")
        .unwrap()
        .current_dir(dir.path())
        .env("PGHOST", "10.255.255.1")
        .env("DATABASE_URL", "postgres://u@10.255.255.1/db")
        .args(["check", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(start.elapsed() < Duration::from_secs(1));
}
