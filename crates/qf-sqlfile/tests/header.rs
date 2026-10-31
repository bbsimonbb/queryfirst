use qf_sqlfile::*;
use std::path::Path;

fn parse(path: &str, src: &str) -> Result<QueryFile, Vec<Diagnostic>> {
    parse_query_file(Path::new(path), &normalise(src))
}

fn ok(path: &str, src: &str) -> QueryFile {
    parse(path, src).unwrap()
}

#[test]
fn pgtyped_name_annotation() {
    let q = ok("x.sql", "/* @name addressBelongsToAccount */\nSELECT 1;");
    assert_eq!(q.name, "addressBelongsToAccount");
    assert!(q.name_from_annotation);
    assert_eq!(q.body, "SELECT 1");
    assert_eq!(q.body_offset, "/* @name addressBelongsToAccount */\n".len());
}

#[test]
fn name_from_file() {
    let q = ok("dir/get-accounts.psql", "SELECT 1");
    assert_eq!(q.name, "getAccounts");
    assert!(!q.name_from_annotation);
    assert_eq!(
        name_from_path(Path::new("insert_chiffrage.sql")),
        "insertChiffrage"
    );
    assert_eq!(name_from_path(Path::new("getAccounts.sql")), "getAccounts");
}

#[test]
fn name_mismatch_warns() {
    // warnings are not errors; parse succeeds, so check via the diagnostics accessor
    let q = ok("bar.psql", "/* @name foo */\nSELECT 1");
    assert_eq!(q.name, "foo");
    assert_eq!(q.diagnostics.len(), 1);
    assert_eq!(q.diagnostics[0].code, "QF003");
    assert_eq!(q.diagnostics[0].severity, Severity::Warning);
}

#[test]
fn expansions() {
    let q = ok(
        "x.sql",
        "/*\n @name q\n @param ids -> (...)\n @param user -> (name, age!)\n @param users -> ((name, age)...)\n*/\nSELECT 1",
    );
    assert_eq!(q.expansions.len(), 3);
    assert_eq!(q.expansions[0].param, "ids");
    assert_eq!(q.expansions[0].kind, ExpansionKind::Spread);
    match &q.expansions[1].kind {
        ExpansionKind::Pick(k) => {
            assert_eq!(
                k[0],
                PickKey {
                    name: "name".into(),
                    required: false
                }
            );
            assert!(k[1].required && k[1].name == "age");
        }
        o => panic!("{o:?}"),
    }
    match &q.expansions[2].kind {
        ExpansionKind::PickSpread(k) => assert_eq!(k.len(), 2),
        o => panic!("{o:?}"),
    }
}

#[test]
fn type_override() {
    let q = ok(
        "x.sql",
        "/*\n @name q\n @type theme -> Themes from \"@/types/user/theme\"\n @type vatCountries -> VatCountries[] from \"@/types/vat\"\n @type :p -> Themes from \"x\"\n*/\nSELECT 1",
    );
    let t = &q.type_overrides;
    assert_eq!(t.len(), 3);
    assert_eq!(t[0].target, OverrideTarget::Column("theme".into()));
    assert_eq!(t[0].type_name, "Themes");
    assert!(!t[0].array);
    assert_eq!(t[0].module.as_deref(), Some("@/types/user/theme"));
    assert_eq!(t[0].line, 3);
    assert!(t[1].array);
    assert_eq!(t[1].type_name, "VatCountries");
    assert_eq!(t[2].target, OverrideTarget::Param("p".into()));
}

#[test]
fn returns() {
    for (s, r) in [
        ("one", Returns::One),
        ("maybe", Returns::Maybe),
        ("many", Returns::Many),
        ("none", Returns::None),
    ] {
        let q = ok("x.sql", &format!("/* @name q\n @returns {s} */\nSELECT 1"));
        assert_eq!(q.returns, Some(r));
    }
    let e = parse("x.sql", "/* @name q\n @returns lots */\nSELECT 1").unwrap_err();
    assert_eq!(e[0].code, "QF001");
    assert_eq!(e[0].line, 2);
}

#[test]
fn description() {
    let q = ok(
        "x.sql",
        "/*\n  Fetches things.\n  @name q\n  More detail.\n*/\nSELECT 1",
    );
    assert_eq!(
        q.description.as_deref(),
        Some("Fetches things.\nMore detail.")
    );
}

#[test]
fn two_queries_rejected() {
    let e = parse(
        "x.sql",
        "/* @name a */\nSELECT 1;\n/* @name b */\nSELECT 2;",
    )
    .unwrap_err();
    assert_eq!(e[0].code, "QF002");
    let e = parse("x.sql", "/* @name a */\nSELECT 1; SELECT 2;").unwrap_err();
    assert_eq!(e[0].code, "QF002");
    let e = parse("x.sql", "SELECT 1;\nSELECT 2").unwrap_err();
    assert_eq!(e[0].code, "QF002");
    // semicolons in strings/comments are fine
    ok("x.sql", "SELECT ';' -- a;b\n;");
}

#[test]
fn no_header_when_plain_comment() {
    let q = ok("x.sql", "/* just a comment */\nSELECT 1");
    assert_eq!(q.description, None);
    assert_eq!(q.body, "/* just a comment */\nSELECT 1");
}

#[test]
fn crlf_equivalent() {
    let lf = "/* @name q\n @param ids -> (...)\n*/\nSELECT 1;\n";
    let crlf = lf.replace('\n', "\r\n");
    assert_eq!(ok("x.sql", lf), ok("x.sql", &crlf));
    assert_eq!(normalise("a\rb\r\nc"), "a\nb\nc");
}
