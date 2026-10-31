//! `qf describe` without a database: describe is faked, the catalog is empty.

use assert_cmd::Command;
use predicates::prelude::*;
use qf_meta::{
    Column, NullableReason, Param, Provider, QueryMeta, RequiredReason, Statement, Status,
    Transform, TypeCategory, TypeRef, from_json,
};
use qf_pg::{Catalog, Described, DescribedColumn, PgQueryError};
use queryfirst::cmd::describe::{Input, choose_input, describe_with, render_text};
use queryfirst::engine::Describe;

fn ty(name: &str, modifier: Option<&str>, array: bool) -> TypeRef {
    TypeRef {
        schema: "pg_catalog".into(),
        name: name.into(),
        array,
        modifier: modifier.map(str::to_string),
        category: TypeCategory::Base,
        enum_values: None,
    }
}

struct Fake;

impl Describe for Fake {
    async fn describe(&self, sql: &str) -> Result<Described, PgQueryError> {
        if sql.contains("FORM") {
            return Err(PgQueryError::Db {
                code: "42601".into(),
                message: "syntax error at or near \"FORM\"".into(),
                detail: None,
                hint: None,
                position: Some(1),
            });
        }
        Ok(Described {
            params: if sql.contains("$1") {
                vec![ty("int4", None, false)]
            } else {
                vec![]
            },
            columns: vec![DescribedColumn {
                name: "id".into(),
                db_type: ty("int4", None, false),
                source: None,
            }],
        })
    }
}

fn run<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(f)
}

fn describe_sql(sql: &str) -> queryfirst::engine::Analyzed {
    let input = Input::Sql(sql.into());
    run(describe_with(
        std::path::Path::new("."),
        &input,
        &Fake,
        &Catalog::default(),
    ))
    .unwrap()
}

fn param(name: &str, t: TypeRef, required: bool, reason: RequiredReason) -> Param {
    Param {
        name: name.into(),
        db_index: vec![1],
        db_type: t,
        required,
        required_reason: reason,
        transform: Transform::Scalar,
        locs: vec![],
        type_override: None,
    }
}

fn column(name: &str, t: TypeRef, nullable: bool, reason: NullableReason) -> Column {
    Column {
        name: name.into(),
        db_type: t,
        nullable,
        nullable_reason: reason,
        source: None,
        type_override: None,
    }
}

fn meta() -> QueryMeta {
    QueryMeta {
        version: 1,
        source: "<sql>".into(),
        name: "adhoc".into(),
        description: None,
        sql_hash: "sha256:x".into(),
        provider: Provider::Postgres,
        statement: Statement {
            source: String::new(),
            db: String::new(),
        },
        params: vec![
            param(
                "id",
                ty("int4", None, false),
                true,
                RequiredReason::InferredWhereEqNotnull,
            ),
            param(
                "tags",
                ty("text", None, true),
                false,
                RequiredReason::DefaultNullable,
            ),
        ],
        columns: vec![
            column(
                "id",
                ty("int4", None, false),
                false,
                NullableReason::ProvenNotnull,
            ),
            column(
                "total",
                ty("numeric", Some("(12,2)"), false),
                true,
                NullableReason::OuterJoin,
            ),
        ],
        returns: None,
        status: Status::Ok,
        generated: vec![],
    }
}

#[test]
fn text_shows_reasons_aligned() {
    let text = render_text(&meta());
    assert!(text.starts_with("adhoc\n"), "{text}");
    assert!(
        text.contains("  :id    int4    required (inferred:where-eq-notnull)\n"),
        "{text}"
    );
    assert!(
        text.contains("  :tags  text[]  optional (default-nullable)\n"),
        "{text}"
    );
    assert!(
        text.contains("  total  numeric(12,2)  nullable (outer-join)\n"),
        "{text}"
    );
    assert!(
        text.contains("  id     int4           not null (proven-notnull)\n"),
        "{text}"
    );
}

#[test]
fn text_with_nothing_says_none() {
    let mut m = meta();
    m.params.clear();
    m.columns.clear();
    let text = render_text(&m);
    assert!(text.contains("params: none\n") && text.contains("columns: none\n"));
}

#[test]
fn sql_input_is_named_adhoc() {
    let a = describe_sql("SELECT id FROM t WHERE id = :id");
    assert_eq!(a.meta.name, "adhoc");
    assert_eq!(a.meta.source, "<sql>");
    assert_eq!(a.meta.params.len(), 1);
    assert!(a.meta.generated.is_empty());
}

#[test]
fn name_header_is_honoured() {
    let a = describe_sql("/* @name getThing */\nSELECT id FROM t");
    assert_eq!(a.meta.name, "getThing");
}

#[test]
fn broken_sql_is_a_diagnostic_not_an_error() {
    let a = describe_sql("SELECT id\nFORM t");
    assert!(matches!(a.meta.status, Status::Broken { .. }));
    assert_eq!(a.meta.name, "adhoc");
    assert!(!a.diagnostics.is_empty());
}

#[test]
fn choose_input_rules() {
    let none = || -> Option<String> { None };
    // Two inputs.
    let e = choose_input(Some("a.sql".as_ref()), Some("select 1"), none).unwrap_err();
    assert_eq!(e.code, "QF091");
    assert!(e.message.contains("exactly one"), "{}", e.message);
    // No input and a terminal.
    let e = choose_input(None, None, none).unwrap_err();
    assert_eq!(e.code, "QF091");
    // Blank piped stdin.
    assert!(choose_input(None, None, || Some("  \n".into())).is_err());
    // Stdin is only read when needed.
    let never = || -> Option<String> { panic!("stdin must not be read") };
    assert!(matches!(
        choose_input(None, Some("select 1"), never).unwrap(),
        Input::Sql(_)
    ));
    assert!(matches!(
        choose_input(Some("a.sql".as_ref()), None, never).unwrap(),
        Input::File(_)
    ));
    assert!(matches!(
        choose_input(None, None, || Some("select 1".into())).unwrap(),
        Input::Sql(s) if s == "select 1"
    ));
}

#[test]
fn two_inputs_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    Command::cargo_bin("qf")
        .unwrap()
        .current_dir(dir.path())
        .args(["describe", "a.sql", "--sql", "select 1"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("QF091").and(predicate::str::contains("exactly one")));
}

#[test]
fn no_input_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    Command::cargo_bin("qf")
        .unwrap()
        .current_dir(dir.path())
        .arg("describe")
        .write_stdin("")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("QF091"));
}

#[test]
fn meta_json_roundtrips() {
    let json = qf_meta::to_canonical_json(&meta());
    assert_eq!(from_json(&json).unwrap(), meta());
}
