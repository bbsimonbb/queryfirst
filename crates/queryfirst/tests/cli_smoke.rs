use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn version_prints() {
    Command::cargo_bin("qf")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"^qf \d+\.\d+\.\d+").unwrap());
}

#[test]
fn subcommands_are_not_implemented() {
    for sub in ["schema", "init"] {
        Command::cargo_bin("qf")
            .unwrap()
            .arg(sub)
            .assert()
            .code(2)
            .stderr(predicate::str::contains("not implemented"));
    }
}

#[test]
fn generate_empty_project_needs_no_database() {
    let dir = tempfile::tempdir().unwrap();
    Command::cargo_bin("qf")
        .unwrap()
        .current_dir(dir.path())
        .env_remove("DATABASE_URL")
        .env_remove("PGHOST")
        .args(["generate", "--json"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("\"command\": \"generate\""));
}

#[test]
fn generate_bad_config_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("queryfirst.json"), "{ \"incldue\": [] }").unwrap();
    Command::cargo_bin("qf")
        .unwrap()
        .current_dir(dir.path())
        .arg("generate")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("queryfirst.json:1:"))
        .stderr(predicate::str::contains("error[QF091]"))
        .stderr(predicate::str::contains("incldue"));
}

#[test]
fn generate_missing_path_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    Command::cargo_bin("qf")
        .unwrap()
        .current_dir(dir.path())
        .args(["generate", "app/[id]/nope.psql"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("error[QF091]"));
}

#[test]
fn generate_without_connection_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.psql"), "SELECT 1 AS one").unwrap();
    let mut c = Command::cargo_bin("qf").unwrap();
    for v in [
        "DATABASE_URL",
        "PGHOST",
        "PGPORT",
        "PGUSER",
        "PGDATABASE",
        "PGPASSWORD",
    ] {
        c.env_remove(v);
    }
    c.current_dir(dir.path())
        .arg("generate")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("error[QF091]"));
    assert!(!dir.path().join("a.qf.json").exists());
}

#[test]
fn generate_unreachable_database_is_qf090_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.psql"), "SELECT 1 AS one").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let out = Command::cargo_bin("qf")
        .unwrap()
        .current_dir(dir.path())
        .env(
            "DATABASE_URL",
            format!("postgres://u:secret@127.0.0.1:{port}/db"),
        )
        .arg("generate")
        .output()
        .unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("error[QF090]"), "{stderr}");
    assert!(stderr.contains(&format!("127.0.0.1:{port}")), "{stderr}");
    assert!(stderr.contains("hint:"), "{stderr}");
    assert!(!stderr.contains("secret"), "{stderr}");
    assert!(!dir.path().join("a.qf.json").exists());
    assert!(!dir.path().join("a.query.ts").exists());
}
