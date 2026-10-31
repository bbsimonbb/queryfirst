//! `qf bundle` without a database: every meta file of the project in one manifest.

use assert_cmd::Command;
use qf_meta::{MANIFEST_FILE, QueryMeta, from_json, manifest_from_json, to_canonical_json};
use std::path::Path;

const FIXTURE: &str = include_str!("../../qf-meta/tests/fixtures/address.qf.json");

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn meta(source: &str) -> QueryMeta {
    let mut m = from_json(FIXTURE).unwrap();
    m.source = source.into();
    m
}

/// A meta next to its (absent) source: bundle reads only meta files.
fn add_meta(root: &Path, source: &str) {
    let rel = format!("{}.qf.json", source.rsplit_once('.').unwrap().0);
    write(root, &rel, &to_canonical_json(&meta(source)));
}

fn bundle(root: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::cargo_bin("qf")
        .unwrap()
        .current_dir(root)
        .env_remove("DATABASE_URL")
        .arg("bundle")
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "queryfirst.json", "{}\n");
    add_meta(root, "z/last.psql");
    add_meta(root, "app/[id]/(queries)/get-order.psql");
    add_meta(root, "app/a.sql");
    add_meta(root, "node_modules/pkg/ignored.psql");
    dir
}

#[test]
fn manifest_contains_all_metas_sorted() {
    let dir = project();
    let root = dir.path();
    let (code, _, stderr) = bundle(root, &[]);
    assert_eq!(code, 0, "{stderr}");
    let text = std::fs::read_to_string(root.join(MANIFEST_FILE)).unwrap();
    let m = manifest_from_json(&text).unwrap();
    let sources: Vec<&str> = m.queries.iter().map(|q| q.source.as_str()).collect();
    assert_eq!(
        sources,
        [
            "app/[id]/(queries)/get-order.psql",
            "app/a.sql",
            "z/last.psql"
        ]
    );
    assert_eq!(m.queries[0], meta("app/[id]/(queries)/get-order.psql"));
    assert_eq!(m.settings.timestamp, None);
    assert!(text.ends_with("}\n") && !text.contains('\r'));
    assert!(stderr.contains("Queryfirst bundle: 3 queries"), "{stderr}");
}

#[test]
fn two_runs_identical_bytes() {
    let dir = project();
    let root = dir.path();
    assert_eq!(bundle(root, &[]).0, 0);
    let a = std::fs::read(root.join(MANIFEST_FILE)).unwrap();
    std::fs::remove_file(root.join(MANIFEST_FILE)).unwrap();
    assert_eq!(bundle(root, &[]).0, 0);
    let b = std::fs::read(root.join(MANIFEST_FILE)).unwrap();
    assert_eq!(a, b);
    // A third run over an existing manifest changes nothing.
    let (code, stdout, _) = bundle(root, &["--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["written"], serde_json::json!([]));
    assert_eq!(std::fs::read(root.join(MANIFEST_FILE)).unwrap(), a);
}

#[test]
fn output_option_and_timestamp_setting() {
    let dir = project();
    let root = dir.path();
    write(root, "queryfirst.json", "{\"timestamp\": \"utc\"}\n");
    let (code, _, stderr) = bundle(root, &["-o", "dist/m.json"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(!root.join(MANIFEST_FILE).exists());
    let m =
        manifest_from_json(&std::fs::read_to_string(root.join("dist/m.json")).unwrap()).unwrap();
    assert_eq!(m.settings.timestamp.as_deref(), Some("utc"));
    assert_eq!(m.queries.len(), 3);
}

#[test]
fn unreadable_meta_fails_and_writes_nothing() {
    let dir = project();
    let root = dir.path();
    write(root, "app/bad.qf.json", "{ not json");
    let (code, stdout, _) = bundle(root, &["--json"]);
    assert_eq!(code, 1);
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["diagnostics"][0]["code"], "QF030");
    assert_eq!(v["diagnostics"][0]["file"], "app/bad.qf.json");
    assert!(!root.join(MANIFEST_FILE).exists());
}

#[test]
fn excluded_metas_left_out() {
    let dir = project();
    let root = dir.path();
    write(
        root,
        "queryfirst.json",
        "{\"exclude\": [\"**/node_modules/**\", \"z/**\"]}\n",
    );
    assert_eq!(bundle(root, &[]).0, 0);
    let m =
        manifest_from_json(&std::fs::read_to_string(root.join(MANIFEST_FILE)).unwrap()).unwrap();
    assert_eq!(m.queries.len(), 2);
}

#[test]
fn bad_config_exit_2() {
    let dir = project();
    let root = dir.path();
    write(root, "queryfirst.json", "{\"nope\": 1}\n");
    assert_eq!(bundle(root, &[]).0, 2);
    assert!(!root.join(MANIFEST_FILE).exists());
}

#[test]
fn orphan_metas_are_skipped_with_a_warning() {
    let dir = project();
    let root = dir.path();
    // The query files of two metas exist; `z/last.psql` was deleted.
    write(root, "app/a.sql", "SELECT 1\n");
    write(root, "app/[id]/(queries)/get-order.psql", "SELECT 1\n");
    let (code, stdout, _) = bundle(root, &["--json"]);
    assert_eq!(code, 0, "{stdout}");
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let d = &v["diagnostics"][0];
    assert_eq!(d["code"], "QF035");
    assert_eq!(d["severity"], "warning");
    assert_eq!(d["file"], "z/last.qf.json");
    assert_eq!(v["summary"]["queries"], 2);
    let m =
        manifest_from_json(&std::fs::read_to_string(root.join(MANIFEST_FILE)).unwrap()).unwrap();
    let sources: Vec<&str> = m.queries.iter().map(|q| q.source.as_str()).collect();
    assert_eq!(sources, ["app/[id]/(queries)/get-order.psql", "app/a.sql"]);
    let (_, _, stderr) = bundle(root, &[]);
    assert!(stderr.contains("warning[QF035]"), "{stderr}");
}
