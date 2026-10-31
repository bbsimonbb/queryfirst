use qf_gen_ts::TypeOverrideOpt;
use queryfirst::config::{Config, load_config, parse_config};

fn write(dir: &std::path::Path, text: &str) {
    std::fs::write(dir.join("queryfirst.json"), text).unwrap();
}

#[test]
fn defaults_without_file() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = load_config(dir.path()).unwrap();
    assert_eq!(cfg, Config::default());
    assert_eq!(cfg.provider, "postgres");
    assert_eq!(cfg.include, vec!["**/*.psql", "**/*.sql"]);
    assert_eq!(cfg.exclude, vec!["**/node_modules/**"]);
    assert_eq!(cfg.pool, 8);
    assert!(cfg.connection.is_none());
    assert!(cfg.timestamp.is_none());
    assert!(cfg.test.is_none());
    assert_eq!(cfg.generators.len(), 1);
    let g = &cfg.generators[0];
    assert_eq!(g.name, "typescript");
    assert_eq!(g.emit_template, "{{dir}}/{{name}}.qf.ts");
    assert!(g.types_overrides.is_empty());
    assert!(!g.camel_case_column_names);
}

#[test]
fn defaults_applied_to_partial_file() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        r#"{ "$schema": "./node_modules/queryfirst/queryfirst.schema.json", "exclude": ["sql-migration/**"], "pool": 3 }"#,
    );
    let cfg = load_config(dir.path()).unwrap();
    assert_eq!(cfg.include, vec!["**/*.psql", "**/*.sql"]);
    assert_eq!(cfg.exclude, vec!["sql-migration/**"]);
    assert_eq!(cfg.pool, 3);
    assert_eq!(cfg.generators, Config::default().generators);
}

#[test]
fn generator_defaults_inside_entry() {
    let cfg = parse_config(
        r#"{ "generators": [ { "name": "typescript", "camelCaseColumnNames": true } ] }"#,
    )
    .unwrap();
    assert_eq!(cfg.generators[0].emit_template, "{{dir}}/{{name}}.qf.ts");
    assert!(cfg.generators[0].camel_case_column_names);
}

#[test]
fn full_file() {
    let cfg = parse_config(
        r#"{
  "provider": "postgres",
  "connection": { "env": "DEV_DATABASE_URL" },
  "timestamp": "utc",
  "test": { "connection": { "env": "PROD_URL", "command": "get-url" } },
  "generators": [ { "name": "typescript", "emitTemplate": "{{dir}}/{{name}}.gen.ts" } ]
}"#,
    )
    .unwrap();
    assert_eq!(cfg.connection.unwrap().env, "DEV_DATABASE_URL");
    assert_eq!(cfg.timestamp.as_deref(), Some("utc"));
    let t = cfg.test.unwrap().connection.unwrap();
    assert_eq!(t.env.as_deref(), Some("PROD_URL"));
    assert_eq!(t.command.as_deref(), Some("get-url"));
    assert_eq!(cfg.generators[0].emit_template, "{{dir}}/{{name}}.gen.ts");
}

#[test]
fn unknown_key_is_qf091_naming_the_key() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "{\n  \"pool\": 2,\n  \"incldue\": []\n}\n");
    let d = load_config(dir.path()).unwrap_err();
    assert_eq!(d.code, "QF091");
    assert!(d.message.contains("incldue"), "{}", d.message);
    assert_eq!(d.line, 3);
}

#[test]
fn unknown_nested_key_is_qf091() {
    let d = parse_config(r#"{ "generators": [ { "name": "typescript", "emitTemplat": "x" } ] }"#)
        .unwrap_err();
    assert_eq!(d.code, "QF091");
    assert!(d.message.contains("emitTemplat"), "{}", d.message);
}

#[test]
fn invalid_values_are_qf091() {
    for text in [
        r#"{ "provider": "mysql" }"#,
        r#"{ "pool": 0 }"#,
        r#"{ "timestamp": "local" }"#,
        r#"{ "generators": [ { "name": "kotlin" } ] }"#,
        r#"{ "include": "**/*.sql" }"#,
        r#"{ "include": ["[unclosed"] }"#,
        "{ not json",
    ] {
        let d = parse_config(text).unwrap_err();
        assert_eq!(d.code, "QF091", "{text}");
    }
}

#[test]
fn types_overrides_accepts_both_pgtyped_shapes() {
    let cfg = parse_config(
        r#"{ "generators": [ { "name": "typescript", "typesOverrides": {
            "numeric": "number",
            "date": { "parameter": "string", "return": "Date" },
            "int8": { "return": "bigint" }
        } } ] }"#,
    )
    .unwrap();
    let o = &cfg.generators[0].types_overrides;
    assert_eq!(o["numeric"], TypeOverrideOpt::Both("number".into()));
    assert_eq!(
        o["date"],
        TypeOverrideOpt::Split {
            parameter: Some("string".into()),
            ret: Some("Date".into())
        }
    );
    assert_eq!(
        o["int8"],
        TypeOverrideOpt::Split {
            parameter: None,
            ret: Some("bigint".into())
        }
    );
    let ts = cfg.generators[0].ts_options();
    assert_eq!(ts.types_overrides, *o);
    assert_eq!(ts.emit_template, "{{dir}}/{{name}}.qf.ts");
}

#[test]
fn schema_documents_every_key() {
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../schema/queryfirst.schema.json")).unwrap();
    let props: Vec<&str> = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        props,
        vec![
            "$schema",
            "provider",
            "include",
            "exclude",
            "connection",
            "pool",
            "timestamp",
            "test",
            "generators"
        ]
    );
    assert_eq!(schema["additionalProperties"], false);
    let generator: Vec<&str> = schema["properties"]["generators"]["items"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        generator,
        vec![
            "name",
            "emitTemplate",
            "typesOverrides",
            "camelCaseColumnNames"
        ]
    );
    // Every documented key is accepted.
    parse_config(
        r#"{ "$schema": "x", "provider": "postgres", "include": [], "exclude": [],
             "connection": { "env": "X" }, "pool": 1, "timestamp": "utc",
             "test": { "connection": { "env": "Y", "command": "z" } },
             "generators": [ { "name": "typescript", "emitTemplate": "{{dir}}/{{name}}.ts",
                               "typesOverrides": {}, "camelCaseColumnNames": true } ] }"#,
    )
    .unwrap();
}
