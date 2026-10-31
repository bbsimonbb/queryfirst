use queryfirst::config::{Config, parse_config};
use queryfirst::discover::discover;
use std::path::{Path, PathBuf};

fn touch(root: &Path, rel: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, "SELECT 1").unwrap();
}

fn posix(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|p| {
            p.components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/")
        })
        .collect()
}

#[test]
fn literal_bracket_paths() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    touch(root, "tmp/app/[id]/(queries)/a.psql");
    touch(root, "tmp/app/i/(queries)/b.psql");
    let cfg = Config::default();
    let arg = root.join("tmp/app/[id]/(queries)/a.psql");
    let found = discover(root, &cfg, &[arg]).unwrap();
    assert_eq!(posix(&found), vec!["tmp/app/[id]/(queries)/a.psql"]);

    // A directory argument is never read as a glob either.
    let found = discover(root, &cfg, &[root.join("tmp/app/[id]")]).unwrap();
    assert_eq!(posix(&found), vec!["tmp/app/[id]/(queries)/a.psql"]);

    // A project-relative argument works as well.
    let found = discover(
        root,
        &cfg,
        &[PathBuf::from("tmp/app/[id]/(queries)/a.psql")],
    )
    .unwrap();
    assert_eq!(posix(&found), vec!["tmp/app/[id]/(queries)/a.psql"]);
}

#[test]
fn whole_project_uses_include() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    touch(root, "a.sql");
    touch(root, "app/b.psql");
    touch(root, "app/c.ts");
    touch(root, "app/[slug]/d.psql");
    let found = discover(root, &Config::default(), &[]).unwrap();
    assert_eq!(
        posix(&found),
        vec!["a.sql", "app/[slug]/d.psql", "app/b.psql"]
    );
}

#[test]
fn exclude_applies() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    touch(root, "app/a.psql");
    touch(root, "node_modules/pkg/x.sql");
    touch(root, "app/node_modules/y.sql");
    touch(root, "sql-migration/001.sql");
    let cfg = parse_config(r#"{ "exclude": ["**/node_modules/**", "sql-migration/**"] }"#).unwrap();
    let found = discover(root, &cfg, &[]).unwrap();
    assert_eq!(posix(&found), vec!["app/a.psql"]);

    // Also when walking a directory argument.
    let found = discover(root, &cfg, &[root.to_path_buf()]).unwrap();
    assert_eq!(posix(&found), vec!["app/a.psql"]);

    // A file argument that is excluded is a usage error, not silently skipped.
    let err = discover(root, &cfg, &[root.join("sql-migration/001.sql")]).unwrap_err();
    assert_eq!(err.code, "QF091");
}

#[test]
fn dir_argument_recurses() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    touch(root, "app/orders/a.psql");
    touch(root, "app/orders/deep/er/b.sql");
    touch(root, "app/other/c.psql");
    let found = discover(root, &Config::default(), &[root.join("app/orders")]).unwrap();
    assert_eq!(
        posix(&found),
        vec!["app/orders/a.psql", "app/orders/deep/er/b.sql"]
    );
}

#[test]
fn duplicates_are_merged_and_sorted() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    touch(root, "app/b.psql");
    touch(root, "app/a.psql");
    let found = discover(
        root,
        &Config::default(),
        &[
            root.join("app/b.psql"),
            root.join("app"),
            root.join("app/a.psql"),
        ],
    )
    .unwrap();
    assert_eq!(posix(&found), vec!["app/a.psql", "app/b.psql"]);
}

#[test]
fn missing_or_outside_paths_are_errors() {
    let dir = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    touch(other.path(), "x.psql");
    let root = dir.path();
    let err = discover(root, &Config::default(), &[root.join("nope.psql")]).unwrap_err();
    assert_eq!(err.code, "QF091");
    let err = discover(root, &Config::default(), &[other.path().join("x.psql")]).unwrap_err();
    assert_eq!(err.code, "QF091");
}

#[test]
fn metas_in_scope() {
    use queryfirst::discover::discover_metas;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for rel in [
        "app/[id]/(queries)/a.qf.json",
        "app/b.qf.json",
        "app/b.psql",
        "app/c.psql",
        "node_modules/x/d.qf.json",
        "other/e.qf.json",
    ] {
        touch(root, rel);
    }
    let cfg = Config::default();
    assert_eq!(
        discover_metas(root, &cfg, &[]).unwrap(),
        [
            "app/[id]/(queries)/a.qf.json",
            "app/b.qf.json",
            "other/e.qf.json"
        ]
    );
    // A directory is walked; a query file stands for its meta, even one not written yet.
    let args = [root.join("app/[id]/(queries)"), root.join("app/c.psql")];
    assert_eq!(
        discover_metas(root, &cfg, &args).unwrap(),
        ["app/[id]/(queries)/a.qf.json", "app/c.qf.json"]
    );
    touch(root, "app/notes.txt");
    let err = discover_metas(root, &cfg, &[root.join("app/notes.txt")]).unwrap_err();
    assert_eq!(err.code, "QF091");
}
