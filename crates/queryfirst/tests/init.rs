use assert_cmd::Command;
use queryfirst::config::{Config, load_config};

const SIMPLYFIRST: &str = r#"{
  "$schema": "./node_modules/queryfirst/queryfirst.schema.json",
  "include": [
    "app/**/*.*sql"
  ],
  "generators": [
    {
      "name": "typescript",
      "emitTemplate": "{{dir}}/{{name}}.qf.ts",
      "typesOverrides": {
        "numeric": "number"
      }
    }
  ]
}
"#;

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/pgtyped/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn qf(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::cargo_bin("qf")
        .unwrap()
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn default_init() {
    let dir = tempfile::tempdir().unwrap();
    let out = qf(dir.path(), &["init"]);
    assert_eq!(out.status.code(), Some(0));
    let text = std::fs::read_to_string(dir.path().join("queryfirst.json")).unwrap();
    assert!(text.contains("\"$schema\""), "{text}");
    assert!(text.ends_with("}\n") && !text.contains('\r'));
    assert_eq!(load_config(dir.path()).unwrap(), Config::default());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Queryfirst init"));
    assert!(
        err.contains(".gitattributes")
            && err.contains("*.qf.ts text eol=lf")
            && err.contains("*.qf.json text eol=lf"),
        "{err}"
    );
    // Not in the JSON output, which stays machine-readable.
    let dir = tempfile::tempdir().unwrap();
    let out = qf(dir.path(), &["init", "--json"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("gitattributes"));
}

#[test]
fn refuses_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("queryfirst.json"), "{}\n").unwrap();
    for args in [&["init"][..], &["init", "--from-pgtyped"]] {
        let out = qf(dir.path(), args);
        assert_eq!(out.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&out.stderr).contains("error[QF091]"));
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("queryfirst.json")).unwrap(),
        "{}\n"
    );
}

#[test]
fn from_pgtyped_json() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("pgtyped-config.json"),
        fixture("pgtyped-config.json"),
    )
    .unwrap();
    let out = qf(dir.path(), &["init", "--from-pgtyped"]);
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    let text = std::fs::read_to_string(dir.path().join("queryfirst.json")).unwrap();
    assert_eq!(text, SIMPLYFIRST);
    load_config(dir.path()).unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    for k in ["failOnError", "maxWorkerThreads", "db"] {
        assert!(err.contains(&format!("`{k}`")), "{err}");
    }
    assert!(
        err.contains("connection comes from PG* env vars or DATABASE_URL; never stored in config")
    );
}

#[test]
fn from_pgtyped_ts_only() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".pgtyped.ts"), fixture("pgtyped.ts.txt")).unwrap();
    let out = qf(dir.path(), &["init", "--from-pgtyped"]);
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    let text = std::fs::read_to_string(dir.path().join("queryfirst.json")).unwrap();
    assert_eq!(text, SIMPLYFIRST);
    let err = String::from_utf8_lossy(&out.stderr);
    // process.env values live under `db`, which is dropped: nothing is reported as unreadable.
    assert!(!err.contains("could not read"), "{err}");
}

#[test]
fn ts_reports_what_it_cannot_read() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".pgtyped.ts"),
        "export default {\n  // comment\n  srcDir: process.env.SRC,\n  camelCaseColumnNames: true,\n  transforms: [{ mode: 'sql', include: '**/*.sql', emitTemplate: '{{dir}}/{{name}}.queries.ts' },],\n}\n",
    )
    .unwrap();
    let out = qf(dir.path(), &["init", "--from-pgtyped"]);
    assert_eq!(out.status.code(), Some(0));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("could not read `srcDir`"), "{err}");
    let text = std::fs::read_to_string(dir.path().join("queryfirst.json")).unwrap();
    assert!(text.contains("\"**/*.sql\""), "{text}");
    assert!(text.contains("\"camelCaseColumnNames\": true"), "{text}");
}

#[test]
fn json_prefers_config_json_and_reports() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("pgtyped-config.json"),
        fixture("pgtyped-config.json"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join(".pgtyped.ts"),
        "export default { srcDir: './other/' }",
    )
    .unwrap();
    let out = qf(dir.path(), &["init", "--from-pgtyped", "--json"]);
    assert_eq!(out.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["command"], "init");
    assert_eq!(v["written"][0], "queryfirst.json");
    let text = std::fs::read_to_string(dir.path().join("queryfirst.json")).unwrap();
    assert_eq!(text, SIMPLYFIRST);
}

#[test]
fn from_pgtyped_without_config_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let out = qf(dir.path(), &["init", "--from-pgtyped"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("error[QF091]"));
    assert!(!dir.path().join("queryfirst.json").exists());
}
