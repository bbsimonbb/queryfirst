//! `qf schema` without a database: rendering and table selection are pure functions over
//! `SchemaInfo`; only the unreachable-database path runs the binary.

use assert_cmd::Command;
use qf_meta::{TypeCategory, TypeRef};
use qf_pg::{ForeignKey, SchemaColumn, SchemaInfo, SchemaTable};
use queryfirst::cmd::schema::{json_list, json_tables, render_list, render_tables, select};

fn ty(schema: &str, name: &str, modifier: Option<&str>, array: bool) -> TypeRef {
    TypeRef {
        schema: schema.into(),
        name: name.into(),
        array,
        modifier: modifier.map(str::to_string),
        category: TypeCategory::Base,
        enum_values: None,
    }
}

fn col(name: &str, t: TypeRef, not_null: bool, default: Option<&str>) -> SchemaColumn {
    SchemaColumn {
        name: name.into(),
        db_type: t,
        not_null,
        not_null_trusted: true,
        default: default.map(str::to_string),
    }
}

fn customers() -> SchemaTable {
    let access = TypeRef {
        category: TypeCategory::Enum,
        enum_values: Some(vec!["edit".into(), "none".into(), "read".into()]),
        ..ty("public", "accessLevel", None, false)
    };
    SchemaTable {
        schema: "public".into(),
        name: "customers".into(),
        kind: 'r',
        columns: vec![
            col(
                "id",
                ty("pg_catalog", "int4", None, false),
                true,
                Some("nextval('customers_id_seq'::regclass)"),
            ),
            col(
                "email",
                ty("pg_catalog", "varchar", Some("(30)"), false),
                true,
                None,
            ),
            col("name", ty("pg_catalog", "text", None, false), false, None),
            col("access", access, true, Some("'read'::\"accessLevel\"")),
            col("tags", ty("pg_catalog", "text", None, true), false, None),
            col(
                "credit",
                ty("pg_catalog", "numeric", Some("(12,2)"), false),
                false,
                None,
            ),
        ],
        primary_key: vec!["id".into()],
        foreign_keys: vec![],
    }
}

fn invoices() -> SchemaTable {
    SchemaTable {
        schema: "public".into(),
        name: "invoices".into(),
        kind: 'r',
        columns: vec![
            col("id", ty("pg_catalog", "int4", None, false), true, None),
            col(
                "customer_id",
                ty("pg_catalog", "int4", None, false),
                true,
                None,
            ),
        ],
        primary_key: vec!["id".into()],
        foreign_keys: vec![ForeignKey {
            columns: vec!["customer_id".into()],
            ref_schema: "public".into(),
            ref_table: "customers".into(),
            ref_columns: vec!["id".into()],
        }],
    }
}

fn recent() -> SchemaTable {
    SchemaTable {
        kind: 'v',
        name: "recent_customers".into(),
        columns: vec![col(
            "id",
            ty("pg_catalog", "int4", None, false),
            false,
            None,
        )],
        primary_key: vec![],
        ..customers()
    }
}

fn info() -> SchemaInfo {
    SchemaInfo {
        tables: vec![customers(), invoices(), recent()],
        search_path: vec!["public".into()],
    }
}

#[test]
fn lists_tables_and_views() {
    assert_eq!(
        render_list(&info()),
        "public.customers (table)\npublic.invoices (table)\npublic.recent_customers (view)\n"
    );
}

#[test]
fn table_detail_text() {
    let expected = "public.customers (table)
  id      int4                NOT NULL  default nextval('customers_id_seq'::regclass)
  email   varchar(30)         NOT NULL
  name    text                null
  access  public.accessLevel  NOT NULL  default 'read'::\"accessLevel\"
  tags    text[]              null
  credit  numeric(12,2)       null
  PK (id)
  enum public.accessLevel: edit | none | read
";
    assert_eq!(render_tables(&[&customers()]), expected);
}

#[test]
fn foreign_keys_and_blank_line_between_tables() {
    let got = render_tables(&[&customers(), &invoices()]);
    assert!(got.contains("\n\npublic.invoices (table)\n"), "{got}");
    assert!(
        got.contains("  FK (customer_id) → public.customers(id)\n"),
        "{got}"
    );
}

#[test]
fn untrusted_not_null_is_marked() {
    let mut t = invoices();
    t.columns[1].not_null_trusted = false;
    let got = render_tables(&[&t]);
    assert!(got.contains("NOT NULL (not enforced everywhere)"), "{got}");
    assert!(
        !got.lines().nth(1).unwrap().contains("not enforced"),
        "{got}"
    );
}

#[test]
fn json_shape() {
    let v = json_tables(&[&customers(), &invoices()]);
    let t = &v["tables"][0];
    assert_eq!(t["schema"], "public");
    assert_eq!(t["name"], "customers");
    assert_eq!(t["kind"], "table");
    assert_eq!(
        t["columns"][0],
        serde_json::json!({
            "name": "id", "type": "int4", "nullable": false,
            "default": "nextval('customers_id_seq'::regclass)"
        })
    );
    assert_eq!(t["columns"][2]["nullable"], true);
    assert_eq!(t["columns"][2]["default"], serde_json::Value::Null);
    assert_eq!(t["columns"][1]["type"], "varchar(30)");
    assert_eq!(t["columns"][4]["type"], "text[]");
    assert_eq!(t["primaryKey"], serde_json::json!(["id"]));
    assert_eq!(
        v["tables"][1]["foreignKeys"],
        serde_json::json!([{
            "columns": ["customer_id"],
            "references": {"table": "public.customers", "columns": ["id"]}
        }])
    );
    assert_eq!(
        v["enums"],
        serde_json::json!([{"name": "public.accessLevel", "values": ["edit", "none", "read"]}])
    );
}

#[test]
fn json_list_has_only_names_and_kinds() {
    let v = json_list(&info());
    assert_eq!(v["tables"].as_array().unwrap().len(), 3);
    assert_eq!(
        v["tables"][2],
        serde_json::json!({"schema": "public", "name": "recent_customers", "kind": "view"})
    );
    assert_eq!(v["enums"], serde_json::json!([]));
}

#[test]
fn select_resolves_and_orders_as_given() {
    let i = info();
    let got = select(&i, &["invoices".into(), "public.customers".into()]).unwrap();
    assert_eq!(got[0].name, "invoices");
    assert_eq!(got[1].name, "customers");
}

#[test]
fn unknown_table_did_you_mean() {
    let i = info();
    let d = select(&i, &["custmers".into()]).unwrap_err();
    assert_eq!(d.code, "QF091");
    assert!(
        d.message.contains(r#"unknown table "custmers""#),
        "{}",
        d.message
    );
    assert!(
        d.message.contains(r#"did you mean "public.customers"?"#),
        "{}",
        d.message
    );
    let d = select(&i, &["zzzzzzzz".into()]).unwrap_err();
    assert!(!d.message.contains("did you mean"), "{}", d.message);
    assert!(d.hint.unwrap().contains("qf schema"));
}

#[test]
fn unreachable_database_is_qf090() {
    let dir = tempfile::tempdir().unwrap();
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
        .arg("schema")
        .output()
        .unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("error[QF090]"), "{stderr}");
    assert!(out.stdout.is_empty());
    assert!(!stderr.contains("secret"), "{stderr}");
}
