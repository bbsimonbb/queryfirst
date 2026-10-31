//! `qf test` without a database: committed metas are built with a fake describe, then tested
//! against another fake standing in for the target database.

use assert_cmd::Command;
use qf_meta::{
    Manifest, ManifestSettings, MetaDiagnostic, NullableReason, QueryMeta, Status, TypeCategory,
    TypeRef, manifest_to_json, to_canonical_json,
};
use qf_pg::{Catalog, ConnSource, Described, DescribedColumn, PgQueryError};
use queryfirst::cmd::test::{
    TestEntry, TestOptions, TestReport, test_connection, test_connection_sources, test_with,
    utc_timezone,
};
use queryfirst::config::{TestConfig, TestConnConfig};
use queryfirst::engine::{Describe, analyze_text};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

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

fn numeric(p: u32, s: u32) -> TypeRef {
    TypeRef {
        modifier: Some(format!("({p},{s})")),
        ..ty("numeric")
    }
}

/// Number of `$n` placeholders (the highest n).
fn placeholders(sql: &str) -> usize {
    let mut max = 0;
    for (i, _) in sql.match_indices('$') {
        let digits: String = sql[i + 1..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(n) = digits.parse::<usize>() {
            max = max.max(n);
        }
    }
    max
}

/// Result column names of `SELECT a, b AS "c!" FROM …` or `… RETURNING a, b`.
fn column_names(sql: &str) -> Vec<String> {
    let list = if let Some(at) = sql.find("RETURNING ") {
        &sql[at + "RETURNING ".len()..]
    } else if let Some(at) = sql.find("SELECT ") {
        let rest = &sql[at + "SELECT ".len()..];
        &rest[..rest.find(" FROM").unwrap_or(rest.len())]
    } else {
        ""
    };
    list.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|item| {
            let name = item.rsplit_once(" AS ").map_or(item, |(_, a)| a);
            let name = name.split_whitespace().next().unwrap_or(name);
            name.rsplit('.')
                .next()
                .unwrap()
                .trim_matches('"')
                .to_string()
        })
        .collect()
}

/// A fake database: params are `int4`; result columns come from the statement text, typed
/// `int4` unless `types` says otherwise; `drop` hides columns, `extra` appends some.
#[derive(Default)]
struct Target {
    types: Vec<(&'static str, TypeRef)>,
    drop: Vec<&'static str>,
    extra: Vec<(&'static str, TypeRef)>,
    param: Option<TypeRef>,
    fail: Option<PgQueryError>,
    calls: AtomicUsize,
}

impl Describe for Target {
    async fn describe(&self, sql: &str) -> Result<Described, PgQueryError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(e) = &self.fail {
            return Err(e.clone());
        }
        let mut columns: Vec<DescribedColumn> = column_names(sql)
            .into_iter()
            .filter(|n| !self.drop.contains(&n.as_str()))
            .map(|n| {
                let db_type = self
                    .types
                    .iter()
                    .find(|(c, _)| *c == n)
                    .map_or_else(|| ty("int4"), |(_, t)| t.clone());
                DescribedColumn {
                    name: n,
                    db_type,
                    source: None,
                }
            })
            .collect();
        columns.extend(self.extra.iter().map(|(n, t)| DescribedColumn {
            name: n.to_string(),
            db_type: t.clone(),
            source: None,
        }));
        Ok(Described {
            params: (0..placeholders(sql))
                .map(|_| self.param.clone().unwrap_or_else(|| ty("int4")))
                .collect(),
            columns,
        })
    }
}

fn dev() -> Target {
    Target {
        types: vec![("total", numeric(12, 2)), ("total!", numeric(12, 2))],
        ..Target::default()
    }
}

async fn generated(source: &str, text: &str) -> QueryMeta {
    let a = analyze_text(
        source,
        Path::new(source),
        text,
        &dev(),
        &Catalog::default(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(a.meta.status, Status::Ok, "{:?}", a.diagnostics);
    a.meta
}

fn entry(meta: QueryMeta) -> TestEntry {
    TestEntry {
        file: meta.source.clone(),
        meta: Ok(meta),
    }
}

fn opts() -> TestOptions {
    TestOptions {
        strict: false,
        timestamp: None,
        pool: 4,
    }
}

async fn test(entries: &[TestEntry], target: &Target, tz: &str, o: &TestOptions) -> TestReport {
    test_with(entries, target, &Catalog::default(), tz, o).await
}

fn codes(r: &TestReport) -> Vec<(String, String)> {
    r.report
        .diagnostics
        .iter()
        .map(|d| (d.code.clone(), d.message.clone()))
        .collect()
}

const ORDER: &str = "/* @name getOrder */\nSELECT id, total, note FROM orders WHERE id = :id\n";

const SHAPES: [(&str, &str); 5] = [
    ("app/get-order.psql", ORDER),
    (
        "app/[id]/(queries)/bang.psql",
        "/* @name bang */\nSELECT id, total AS \"total!\", note AS \"note?\" FROM orders WHERE id = :id! AND x = :x? -- trailing :fake\n",
    ),
    (
        "app/spread.psql",
        "/*\n @name byIds\n @param ids -> (...)\n*/\nSELECT id FROM t WHERE id IN :ids /* block :fake */ AND y = 'é:z'\n",
    ),
    (
        "app/pick.sql",
        "/*\n @name ins\n @param row -> (name!, age)\n @param rows -> ((name, age)...)\n*/\nINSERT INTO t (name, age) VALUES :row, :rows RETURNING id\n",
    ),
    (
        "app/typed.psql",
        "/*\n @name typed\n @type status -> Status from \"@/types\"\n @type :id -> OrderId from \"@/ids\"\n*/\nSELECT status FROM t WHERE id = :id;\n",
    ),
];

#[tokio::test]
async fn unchanged_target_passes_every_shape() {
    let mut entries = Vec::new();
    for (source, text) in SHAPES {
        entries.push(entry(generated(source, text).await));
    }
    let r = test(&entries, &dev(), "UTC", &opts()).await;
    assert_eq!(codes(&r), vec![]);
    assert_eq!(r.exit_code(), 0);
    assert_eq!(r.report.summary.queries, 5);
    let human = r.render_human();
    assert_eq!(
        human,
        "Queryfirst test: 5 queries, 0 with differences; 0 errors, 0 warnings, 0 notes\n"
    );
}

#[tokio::test]
async fn annotations_are_reapplied_on_target() {
    // `:id!`, `"total!"`, `"note?"` and the pick key `name!` are the author's: re-applied on the
    // target, they never show up as differences, even with --strict.
    let m = generated(SHAPES[1].0, SHAPES[1].1).await;
    assert!(m.columns.iter().any(|c| c.name == "total" && !c.nullable));
    let strict = TestOptions {
        strict: true,
        ..opts()
    };
    let r = test(&[entry(m)], &dev(), "UTC", &strict).await;
    assert_eq!(codes(&r), vec![]);
    assert_eq!(r.exit_code(), 0);
}

#[tokio::test]
async fn dropped_column_fails_qf040() {
    let m = generated("app/get-order.psql", ORDER).await;
    let target = Target {
        drop: vec!["note"],
        ..dev()
    };
    let r = test(&[entry(m)], &target, "UTC", &opts()).await;
    assert_eq!(
        codes(&r),
        vec![(
            "QF040".to_string(),
            "getOrder: column \"note\" missing".to_string()
        )]
    );
    assert_eq!(r.exit_code(), 1);
    assert!(
        r.render_human()
            .starts_with("app/get-order.psql: error[QF040]: getOrder: column \"note\" missing\n"),
        "{}",
        r.render_human()
    );
}

#[tokio::test]
async fn widened_numeric_fails_qf041_message() {
    let m = generated("app/get-order.psql", ORDER).await;
    let target = Target {
        types: vec![("total", numeric(14, 2))],
        ..Target::default()
    };
    let r = test(&[entry(m)], &target, "UTC", &opts()).await;
    assert_eq!(r.report.diagnostics[0].code, "QF041");
    assert!(
        r.report.diagnostics[0]
            .message
            .contains("column \"total\" numeric(12,2) → numeric(14,2)"),
        "{:?}",
        r.report.diagnostics
    );
    assert_eq!(r.exit_code(), 1);
}

#[tokio::test]
async fn became_nullable_fails_qf042() {
    let mut m = generated("app/get-order.psql", ORDER).await;
    // As generated against a dev DB that proved `id` NOT NULL.
    m.columns[0].nullable = false;
    m.columns[0].nullable_reason = NullableReason::ProvenNotnull;
    let r = test(&[entry(m)], &dev(), "UTC", &opts()).await;
    assert_eq!(
        codes(&r),
        vec![(
            "QF042".to_string(),
            "getOrder: column \"id\" became nullable".to_string()
        )]
    );
    assert_eq!(r.exit_code(), 1);
}

#[tokio::test]
async fn param_type_change_fails_qf045() {
    let m = generated("app/get-order.psql", ORDER).await;
    let target = Target {
        param: Some(ty("int8")),
        ..dev()
    };
    let r = test(&[entry(m)], &target, "UTC", &opts()).await;
    assert_eq!(
        codes(&r),
        vec![(
            "QF045".to_string(),
            "getOrder: parameter :id int4 → int8".to_string()
        )]
    );
    assert_eq!(r.exit_code(), 1);
}

#[tokio::test]
async fn extra_column_is_a_note_strict_fails() {
    let m = generated("app/get-order.psql", ORDER).await;
    let target = Target {
        extra: vec![("added", ty("text"))],
        ..dev()
    };
    let r = test(&[entry(m.clone())], &target, "UTC", &opts()).await;
    assert_eq!(r.report.diagnostics.len(), 1);
    assert_eq!(r.report.diagnostics[0].code, "QF043");
    assert_eq!(r.exit_code(), 0);
    assert!(r.render_human().contains("note[QF043]"));
    let strict = TestOptions {
        strict: true,
        ..opts()
    };
    let r = test(&[entry(m)], &target, "UTC", &strict).await;
    assert_eq!(r.exit_code(), 1);
    let v = r.to_json();
    assert_eq!(v["ok"], false);
    assert_eq!(v["strict"], true);
    assert_eq!(v["summary"]["notes"], 1);
}

#[tokio::test]
async fn broken_meta_qf049_without_describe() {
    let mut m = generated("app/get-order.psql", ORDER).await;
    m.status = Status::Broken {
        diagnostics: vec![MetaDiagnostic {
            code: "QF010".into(),
            severity: qf_meta::Severity::Error,
            message: "column \"nope\" does not exist".into(),
            hint: None,
            line: 2,
            col: 8,
        }],
    };
    let target = dev();
    let unreadable = TestEntry {
        file: "app/bad.qf.json".into(),
        meta: Err("invalid meta file: EOF".into()),
    };
    let r = test(&[entry(m), unreadable], &target, "UTC", &opts()).await;
    assert_eq!(target.calls.load(Ordering::SeqCst), 0);
    let c = codes(&r);
    assert_eq!(c.len(), 2, "{c:?}");
    assert!(c.iter().all(|(code, _)| code == "QF049"));
    assert!(c[0].1.contains("app/bad.qf.json"), "{c:?}");
    assert!(c[1].1.contains("column \"nope\" does not exist"), "{c:?}");
    assert_eq!(r.exit_code(), 1);
}

#[tokio::test]
async fn inconsistent_meta_qf049() {
    let mut m = generated("app/get-order.psql", ORDER).await;
    m.statement.db = m.statement.db.replace("orders", "other");
    let r = test(&[entry(m)], &dev(), "UTC", &opts()).await;
    assert_eq!(r.report.diagnostics[0].code, "QF049");
    assert_eq!(r.exit_code(), 1);
}

#[tokio::test]
async fn prepare_failure_qf046() {
    let m = generated("app/get-order.psql", ORDER).await;
    let target = Target {
        fail: Some(PgQueryError::Db {
            code: "42P01".into(),
            message: "la relation « orders » n'existe pas".into(),
            detail: None,
            hint: None,
            position: Some(29),
        }),
        ..dev()
    };
    let r = test(&[entry(m)], &target, "UTC", &opts()).await;
    assert_eq!(r.report.diagnostics.len(), 1);
    let d = &r.report.diagnostics[0];
    assert_eq!(d.code, "QF046");
    assert!(
        d.message
            .starts_with("getOrder: does not prepare on the target database: ")
            && d.message.ends_with(r#"relation "orders" does not exist"#),
        "{}",
        d.message
    );
    assert_eq!(r.exit_code(), 1);
}

#[tokio::test]
async fn connection_lost_aborts_exit_2() {
    let a = generated("app/get-order.psql", ORDER).await;
    let mut b = a.clone();
    b.source = "app/b.psql".into();
    let target = Target {
        fail: Some(PgQueryError::Connection("connection reset by peer".into())),
        ..dev()
    };
    let r = test(&[entry(a), entry(b)], &target, "UTC", &opts()).await;
    assert_eq!(r.report.diagnostics.len(), 1, "{:?}", r.report.diagnostics);
    assert_eq!(r.report.diagnostics[0].code, "QF090");
    assert_eq!(r.exit_code(), 2);
}

#[tokio::test]
async fn timezone_check_qf048() {
    let m = generated("app/get-order.psql", ORDER).await;
    let utc = TestOptions {
        timestamp: Some("utc".into()),
        ..opts()
    };
    let r = test(&[entry(m.clone())], &dev(), "Europe/Paris", &utc).await;
    assert_eq!(codes(&r).len(), 1);
    let d = &r.report.diagnostics[0];
    assert_eq!((d.code.as_str(), d.file.as_deref()), ("QF048", None));
    assert!(d.message.contains("Europe/Paris"), "{}", d.message);
    assert_eq!(r.exit_code(), 1);

    for tz in ["UTC", "Etc/UTC", "utc"] {
        let r = test(&[entry(m.clone())], &dev(), tz, &utc).await;
        assert_eq!(codes(&r), vec![], "{tz}");
    }
    // Without `timestamp: "utc"` the time zone is not checked.
    let r = test(&[entry(m)], &dev(), "Europe/Paris", &opts()).await;
    assert_eq!(codes(&r), vec![]);
}

#[test]
fn utc_names() {
    for tz in [
        "UTC",
        "Etc/UTC",
        "UCT",
        "Etc/UCT",
        "Universal",
        "Etc/Universal",
        "Zulu",
        "Etc/Zulu",
        "utc",
    ] {
        assert!(utc_timezone(tz), "{tz}");
    }
    for tz in ["Europe/Paris", "GMT", "Europe/London", "", "UTC+1", "+00"] {
        assert!(!utc_timezone(tz), "{tz}");
    }
}

#[tokio::test]
async fn report_groups_by_query_in_semantic_order() {
    let a = generated("app/get-order.psql", ORDER).await;
    let mut b = a.clone();
    b.source = "app/a-first.psql".into();
    b.name = "aFirst".into();
    let target = Target {
        drop: vec!["id"],
        types: vec![("total", numeric(14, 2))],
        extra: vec![("added", ty("text"))],
        ..Target::default()
    };
    let r = test(&[entry(a), entry(b)], &target, "UTC", &opts()).await;
    let lines: Vec<(Option<String>, String)> = r
        .report
        .diagnostics
        .iter()
        .map(|d| (d.file.clone(), d.code.clone()))
        .collect();
    let a = Some("app/a-first.psql".to_string());
    let g = Some("app/get-order.psql".to_string());
    assert_eq!(
        lines,
        vec![
            (a.clone(), "QF040".into()),
            (a.clone(), "QF041".into()),
            (a, "QF043".into()),
            (g.clone(), "QF040".into()),
            (g.clone(), "QF041".into()),
            (g, "QF043".into()),
        ]
    );
    let human = r.render_human();
    assert!(
        human.ends_with(
            "Queryfirst test: 2 queries, 2 with differences; 4 errors, 0 warnings, 2 notes\n"
        ),
        "{human}"
    );
    let v = r.to_json();
    assert_eq!(v["command"], "test");
    assert_eq!(v["summary"]["differing"], 2);
    assert_eq!(v["diagnostics"].as_array().unwrap().len(), 6);
}

#[test]
fn connection_order() {
    let cfg = TestConfig {
        connection: Some(TestConnConfig {
            env: Some("APP_DB".into()),
            command: Some("node db-url.js".into()),
        }),
    };
    assert_eq!(
        test_connection_sources(Some(&cfg), Some("postgres://x")),
        vec![
            ConnSource::Explicit("postgres://x".into()),
            ConnSource::Env("APP_DB".into()),
            ConnSource::StandardEnv,
            ConnSource::Command("node db-url.js".into()),
        ]
    );
    assert_eq!(
        test_connection_sources(None, None),
        vec![ConnSource::StandardEnv]
    );
}

// ---- The command line, without a database ----

const SECRET: &str = "hunter2secret";

fn qf(dir: &Path) -> Command {
    let mut c = Command::cargo_bin("qf").unwrap();
    c.current_dir(dir);
    for v in [
        "DATABASE_URL",
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
    c
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn manifest_with_one(root: &Path) {
    let meta =
        qf_meta::from_json(include_str!("../../qf-meta/tests/fixtures/address.qf.json")).unwrap();
    let m = Manifest::new(ManifestSettings::default(), vec![meta]);
    write(root, "deploy/m.json", &manifest_to_json(&m));
}

fn output(c: &mut Command) -> (i32, String, String) {
    let o = c.output().unwrap();
    (
        o.status.code().unwrap(),
        String::from_utf8(o.stdout).unwrap(),
        String::from_utf8(o.stderr).unwrap(),
    )
}

#[test]
fn missing_manifest_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    let (code, _, stderr) = output(qf(dir.path()).args(["test", "--manifest", "nope.json"]));
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("QF091"), "{stderr}");
}

#[test]
fn no_connection_configured_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    manifest_with_one(dir.path());
    let (code, _, stderr) = output(qf(dir.path()).args(["test", "--manifest", "deploy/m.json"]));
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("error[QF091]: no database connection configured"),
        "{stderr}"
    );
}

#[test]
fn unreachable_db_never_prints_the_connection_string() {
    let dir = tempfile::tempdir().unwrap();
    manifest_with_one(dir.path());
    let url = format!("postgres://app:{SECRET}@127.0.0.1:1/appdb?sslmode=disable");
    for json in [false, true] {
        let mut c = qf(dir.path());
        c.args(["test", "--manifest", "deploy/m.json", "--db", &url]);
        if json {
            c.arg("--json");
        }
        let (code, stdout, stderr) = output(&mut c);
        assert_eq!(code, 2, "{stderr}");
        assert!(
            format!("{stdout}{stderr}").contains("QF090"),
            "{stdout}{stderr}"
        );
        for out in [&stdout, &stderr] {
            assert!(!out.contains(SECRET), "{out}");
            assert!(!out.contains("postgres://"), "{out}");
        }
    }
}

#[test]
fn command_helper_from_cwd_config_url_never_printed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    manifest_with_one(root);
    // No query files, no meta files: the manifest is all `qf test --manifest` needs, and the
    // connection comes from the queryfirst.json in the current directory.
    write(
        root,
        "queryfirst.json",
        &format!(
            "{{\"test\": {{\"connection\": {{\"command\": \"echo postgres://app:{SECRET}@127.0.0.1:1/appdb?sslmode=disable\"}}}}}}\n"
        ),
    );
    let (code, stdout, stderr) = output(qf(root).args(["test", "--manifest", "deploy/m.json"]));
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("error[QF090]: cannot reach database at 127.0.0.1:1"),
        "{stderr}"
    );
    for out in [&stdout, &stderr] {
        assert!(!out.contains(SECRET), "{out}");
        assert!(!out.contains("postgres://"), "{out}");
    }
}

#[test]
fn tree_mode_reads_metas_then_connects() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let meta =
        qf_meta::from_json(include_str!("../../qf-meta/tests/fixtures/address.qf.json")).unwrap();
    write(root, "queryfirst.json", "{}\n");
    write(root, "app/q.qf.json", &to_canonical_json(&meta));
    let (code, _, stderr) =
        output(qf(root).args(["test", "app", "--db", "postgres://u@127.0.0.1:1/x"]));
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("QF090"), "{stderr}");
    // A path that does not exist is a usage error, before any connection is made.
    let (code, _, stderr) =
        output(qf(root).args(["test", "missing-dir", "--db", "postgres://u@127.0.0.1:1/x"]));
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("error[QF091]: no such file or directory"),
        "{stderr}"
    );
}

#[tokio::test]
async fn pick_key_type_change_fails_qf045() {
    let m = generated(SHAPES[3].0, SHAPES[3].1).await;
    let target = Target {
        param: Some(ty("int8")),
        ..dev()
    };
    let r = test(&[entry(m)], &target, "UTC", &opts()).await;
    let msgs: Vec<String> = codes(&r)
        .into_iter()
        .map(|(c, m)| format!("{c} {m}"))
        .collect();
    assert_eq!(
        msgs,
        [
            "QF045 ins: parameter :row.name int4 → int8",
            "QF045 ins: parameter :row.age int4 → int8",
            "QF045 ins: parameter :rows.name int4 → int8",
            "QF045 ins: parameter :rows.age int4 → int8",
        ]
    );
    assert_eq!(r.exit_code(), 1);
}

#[test]
fn manifest_path_filter_with_relative_project() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // The fixture's source is app/api/(shared-queries)/address.psql, relative to `deploy`.
    manifest_with_one(root);
    let db = "postgres://u@127.0.0.1:1/x";
    let base = [
        "--project",
        "deploy",
        "test",
        "--manifest",
        "deploy/m.json",
        "--db",
        db,
    ];
    // In scope: the run gets as far as connecting.
    for filter in ["deploy/app", "deploy/./app/api/../api", "deploy"] {
        let (code, _, stderr) = output(qf(root).args(base).arg(filter));
        assert_eq!(code, 2, "{filter}: {stderr}");
        assert!(stderr.contains("QF090"), "{filter}: {stderr}");
    }
    // Nothing there: a usage error, not "outside the project".
    let (code, _, stderr) = output(qf(root).args(base).arg("deploy/other"));
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("no query in the manifest is in other"),
        "{stderr}"
    );
    let (_, _, stderr) = output(qf(root).args(base).arg("elsewhere"));
    assert!(
        stderr.contains("is outside the project directory"),
        "{stderr}"
    );
}

#[test]
fn test_sessions_are_read_only() {
    let c = test_connection(
        None,
        Some("postgres://u@h:5432/db?options=-c%20search_path%3Dapp"),
    )
    .unwrap();
    assert_eq!(
        c.options(),
        Some("-c search_path=app -c default_transaction_read_only=on")
    );
}
