use qf_meta::*;

const FIXTURE: &str = include_str!("fixtures/address.qf.json");

fn meta(source: &str, name: &str) -> QueryMeta {
    let mut m = from_json(FIXTURE).unwrap();
    m.source = source.into();
    m.name = name.into();
    m
}

#[test]
fn sorted_by_source() {
    let m = Manifest::new(
        ManifestSettings::default(),
        vec![
            meta("z/a.psql", "a"),
            meta("a/b.psql", "b"),
            meta("m.sql", "c"),
        ],
    );
    let sources: Vec<&str> = m.queries.iter().map(|q| q.source.as_str()).collect();
    assert_eq!(sources, ["a/b.psql", "m.sql", "z/a.psql"]);
    assert_eq!(m.version, MANIFEST_VERSION);
}

#[test]
fn canonical_roundtrip_and_shape() {
    let m = Manifest::new(
        ManifestSettings {
            timestamp: Some("utc".into()),
        },
        vec![meta("b.psql", "b"), meta("a.psql", "a")],
    );
    let s = manifest_to_json(&m);
    assert!(s.ends_with("}\n"));
    assert!(!s.contains('\r'));
    assert!(s.starts_with("{\n  \"version\": 1,\n  \"settings\": {\n    \"timestamp\": \"utc\"\n  },\n  \"queries\": ["), "{s}");
    let back = manifest_from_json(&s).unwrap();
    assert_eq!(back, m);
    assert_eq!(manifest_to_json(&back), s);
}

#[test]
fn no_timestamp_is_omitted() {
    let s = manifest_to_json(&Manifest::new(ManifestSettings::default(), vec![]));
    assert_eq!(
        s,
        "{\n  \"version\": 1,\n  \"settings\": {},\n  \"queries\": []\n}\n"
    );
}

#[test]
fn future_versions_rejected() {
    let s = manifest_to_json(&Manifest::new(ManifestSettings::default(), vec![]));
    let err = manifest_from_json(&s.replace("\"version\": 1", "\"version\": 2")).unwrap_err();
    assert!(err.to_string().contains("newer Queryfirst"), "{err}");

    let s = manifest_to_json(&Manifest::new(
        ManifestSettings::default(),
        vec![meta("a.psql", "a")],
    ));
    // The query's own version, indented inside `queries`.
    let s = s.replace("      \"version\": 1,", "      \"version\": 9,");
    let err = manifest_from_json(&s).unwrap_err();
    assert!(err.to_string().contains("newer Queryfirst"), "{err}");
}

#[test]
fn invalid_json_rejected() {
    assert!(manifest_from_json("{").is_err());
    assert!(manifest_from_json("{\"version\": 1}").is_err());
}

#[test]
fn schema_is_valid_json_and_lists_top_level_keys() {
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../schema/queryfirst.manifest.v1.schema.json")).unwrap();
    let props = schema["properties"].as_object().unwrap();
    for k in ["version", "settings", "queries"] {
        assert!(props.contains_key(k), "schema lacks {k}");
    }
}
