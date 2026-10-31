use qf_meta::*;
use std::path::Path;

const FIXTURE: &str = include_str!("fixtures/address.qf.json");

#[test]
fn canonical_roundtrip() {
    let meta = from_json(FIXTURE).unwrap();
    assert_eq!(to_canonical_json(&meta), FIXTURE);
}

#[test]
fn stable_bytes() {
    let a = to_canonical_json(&from_json(FIXTURE).unwrap());
    let b = to_canonical_json(&from_json(FIXTURE).unwrap());
    assert_eq!(a, b);
    assert!(a.ends_with("}\n"));
    assert!(!a.contains('\r'));
}

fn keys(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::Object(m) => {
            for (k, v) in m {
                out.push(k.clone());
                keys(v, out);
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|v| keys(v, out)),
        _ => {}
    }
}

#[test]
fn no_env_fields() {
    let v: serde_json::Value =
        serde_json::from_str(&to_canonical_json(&from_json(FIXTURE).unwrap())).unwrap();
    let mut ks = Vec::new();
    keys(&v, &mut ks);
    for bad in ["oid", "host", "database", "serverVersion"] {
        assert!(!ks.iter().any(|k| k == bad), "env field {bad} present");
    }
}

#[test]
fn future_version_rejected() {
    let s = FIXTURE.replace("\"version\": 1", "\"version\": 2");
    let err = from_json(&s).unwrap_err();
    assert!(err.to_string().contains("newer Queryfirst"), "{err}");
}

#[test]
fn hash_is_line_ending_independent() {
    let lf = "SELECT 1\n;\n";
    let crlf = "SELECT 1\r\n;\r\n";
    assert_eq!(
        sha256_tag(qf_sqlfile::normalise(crlf).as_bytes()),
        sha256_tag(lf.as_bytes())
    );
    assert!(sha256_tag(b"").starts_with("sha256:e3b0c442"));
}

#[test]
fn broken_status_and_returns_roundtrip() {
    let mut meta = from_json(FIXTURE).unwrap();
    meta.status = Status::Broken {
        diagnostics: vec![MetaDiagnostic {
            code: "QF001".into(),
            severity: Severity::Error,
            message: "boom".into(),
            hint: None,
            line: 1,
            col: 2,
        }],
    };
    meta.returns = Some(Returns::Maybe);
    let json = to_canonical_json(&meta);
    assert!(json.contains("\"state\": \"broken\""));
    assert!(json.contains("\"returns\": \"maybe\""));
    assert_eq!(from_json(&json).unwrap(), meta);
}

#[test]
fn transforms_roundtrip() {
    let t = Transform::PickSpread {
        keys: vec![PickKeyMeta {
            name: "a".into(),
            required: true,
            db_index: 1,
            db_type: from_json(FIXTURE).unwrap().columns[0].db_type.clone(),
        }],
    };
    let s = serde_json::to_string(&t).unwrap();
    assert!(s.starts_with("{\"type\":\"pickSpread\",\"keys\":["), "{s}");
    assert_eq!(serde_json::from_str::<Transform>(&s).unwrap(), t);
}

#[test]
fn meta_path() {
    assert_eq!(
        meta_path_for(Path::new("a/foo.psql")),
        Path::new("a/foo.qf.json")
    );
}

#[test]
fn schema_is_valid_json_and_lists_top_level_keys() {
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../schema/qf-meta.v1.schema.json")).unwrap();
    let props = schema["properties"].as_object().unwrap();
    let fixture: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
    for k in fixture.as_object().unwrap().keys() {
        assert!(props.contains_key(k), "schema lacks {k}");
    }
}
