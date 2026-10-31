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
    for sub in ["generate", "check", "test", "bundle", "schema", "init"] {
        Command::cargo_bin("qf")
            .unwrap()
            .arg(sub)
            .assert()
            .code(2)
            .stderr(predicate::str::contains("not implemented"));
    }
}
