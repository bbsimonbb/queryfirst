use qf_meta::*;

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

fn col(name: &str, t: TypeRef, nullable: bool) -> Column {
    Column {
        name: name.into(),
        db_type: t,
        nullable,
        nullable_reason: NullableReason::ProvenNotnull,
        source: None,
        type_override: None,
    }
}

fn param(name: &str, t: TypeRef) -> Param {
    Param {
        name: name.into(),
        db_index: vec![1],
        db_type: t,
        required: true,
        required_reason: RequiredReason::Annotation,
        transform: Transform::Scalar,
        locs: vec![],
        type_override: None,
    }
}

fn meta(params: Vec<Param>, columns: Vec<Column>) -> QueryMeta {
    QueryMeta {
        version: META_VERSION,
        source: "q.sql".into(),
        name: "getInvoice".into(),
        description: None,
        sql_hash: "sha256:0".into(),
        provider: Provider::Postgres,
        statement: Statement {
            source: String::new(),
            db: String::new(),
        },
        params,
        columns,
        returns: Some(Returns::Many),
        status: Status::Ok,
        generated: vec![],
    }
}

fn enum_ty(values: &[&str]) -> TypeRef {
    TypeRef {
        schema: "public".into(),
        name: "accessLevel".into(),
        array: false,
        modifier: None,
        category: TypeCategory::Enum,
        enum_values: Some(values.iter().map(|s| s.to_string()).collect()),
    }
}

fn one(d: Vec<Difference>, code: &str, sev: Severity, msg: &str) {
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].code, code);
    assert_eq!(d[0].severity, sev);
    assert_eq!(d[0].message, msg);
}

#[test]
fn identical_is_empty() {
    let m = meta(
        vec![param("id", ty("int4"))],
        vec![col("a", ty("int4"), false)],
    );
    assert!(compare(&m, &m.params, &m.columns).is_empty());
}

#[test]
fn extra_column_note() {
    let m = meta(vec![], vec![col("a", ty("int4"), false)]);
    let actual = vec![col("a", ty("int4"), false), col("b", ty("text"), true)];
    one(
        compare(&m, &[], &actual),
        "QF043",
        Severity::Note,
        "getInvoice: column \"b\" text added",
    );
}

#[test]
fn missing_column_error() {
    let m = meta(
        vec![],
        vec![col("a", ty("int4"), false), col("b", ty("text"), true)],
    );
    let actual = vec![col("a", ty("int4"), false)];
    one(
        compare(&m, &[], &actual),
        "QF040",
        Severity::Error,
        "getInvoice: column \"b\" missing",
    );
}

#[test]
fn type_change_error() {
    let mut e = ty("numeric");
    e.modifier = Some("(12,2)".into());
    let mut a = ty("numeric");
    a.modifier = Some("(14,2)".into());
    let m = meta(vec![], vec![col("total", e, false)]);
    one(
        compare(&m, &[], &[col("total", a, false)]),
        "QF041",
        Severity::Error,
        "getInvoice: column \"total\" numeric(12,2) → numeric(14,2)",
    );
    let mut arr = ty("text");
    arr.array = true;
    let m = meta(vec![], vec![col("t", ty("text"), false)]);
    one(
        compare(&m, &[], &[col("t", arr, false)]),
        "QF041",
        Severity::Error,
        "getInvoice: column \"t\" text → text[]",
    );
    let mut sch = ty("text");
    sch.schema = "public".into();
    one(
        compare(&m, &[], &[col("t", sch, false)]),
        "QF041",
        Severity::Error,
        "getInvoice: column \"t\" text → public.text",
    );
}

#[test]
fn became_nullable_error() {
    let m = meta(vec![], vec![col("note", ty("text"), false)]);
    one(
        compare(&m, &[], &[col("note", ty("text"), true)]),
        "QF042",
        Severity::Error,
        "getInvoice: column \"note\" became nullable",
    );
}

#[test]
fn became_notnull_note() {
    let m = meta(vec![], vec![col("note", ty("text"), true)]);
    one(
        compare(&m, &[], &[col("note", ty("text"), false)]),
        "QF044",
        Severity::Note,
        "getInvoice: column \"note\" became non-null",
    );
}

#[test]
fn param_added_removed_changed_error() {
    let m = meta(
        vec![param("id", ty("int4")), param("gone", ty("text"))],
        vec![],
    );
    let actual = vec![param("id", ty("int8")), param("extra", ty("bool"))];
    let d = compare(&m, &actual, &[]);
    assert_eq!(d.len(), 3, "{d:?}");
    assert!(
        d.iter()
            .all(|x| x.code == "QF045" && x.severity == Severity::Error)
    );
    assert_eq!(d[0].message, "getInvoice: parameter :id int4 → int8");
    assert_eq!(d[1].message, "getInvoice: parameter :gone removed");
    assert_eq!(d[2].message, "getInvoice: parameter :extra bool added");
}

#[test]
fn enum_value_removed_error() {
    let m = meta(vec![], vec![col("lvl", enum_ty(&["admin", "user"]), false)]);
    one(
        compare(&m, &[], &[col("lvl", enum_ty(&["user"]), false)]),
        "QF047",
        Severity::Error,
        "getInvoice: column \"lvl\" public.accessLevel lost values: admin",
    );
}

#[test]
fn enum_value_added_warning() {
    let m = meta(vec![], vec![col("lvl", enum_ty(&["user"]), false)]);
    one(
        compare(&m, &[], &[col("lvl", enum_ty(&["admin", "user"]), false)]),
        "QF047",
        Severity::Warning,
        "getInvoice: column \"lvl\" public.accessLevel gained values: admin",
    );
}

#[test]
fn matched_by_name() {
    let m = meta(
        vec![param("a", ty("int4")), param("b", ty("text"))],
        vec![col("x", ty("int4"), false), col("y", ty("text"), true)],
    );
    let p = vec![param("b", ty("text")), param("a", ty("int4"))];
    let c = vec![col("y", ty("text"), true), col("x", ty("int4"), false)];
    assert!(compare(&m, &p, &c).is_empty());
}

#[test]
fn category_change_error() {
    let mut e = enum_ty(&["a"]);
    e.name = "status".into();
    let mut a = e.clone();
    a.category = TypeCategory::Domain;
    a.enum_values = None;
    let m = meta(vec![], vec![col("x", e, false)]);
    one(
        compare(&m, &[], &[col("x", a, false)]),
        "QF041",
        Severity::Error,
        "getInvoice: column \"x\" public.status (enum) → public.status (domain)",
    );
}

#[test]
fn enum_values_some_to_none_error() {
    let e = enum_ty(&["a", "b"]);
    let mut a = e.clone();
    a.enum_values = None;
    let m = meta(vec![], vec![col("x", e, false)]);
    one(
        compare(&m, &[], &[col("x", a, false)]),
        "QF041",
        Severity::Error,
        "getInvoice: column \"x\" public.accessLevel enum values [a, b] → none",
    );
}

#[test]
fn type_and_nullability_change_two_differences() {
    let m = meta(vec![], vec![col("x", ty("int4"), false)]);
    let d = compare(&m, &[], &[col("x", ty("int8"), true)]);
    assert_eq!(d.len(), 2, "{d:?}");
    assert_eq!(d[0].code, "QF041");
    assert_eq!(d[1].code, "QF042");
}

#[test]
fn enum_lost_and_gained_two_differences() {
    let m = meta(vec![], vec![col("x", enum_ty(&["a", "b"]), false)]);
    let d = compare(&m, &[], &[col("x", enum_ty(&["b", "c"]), false)]);
    assert_eq!(d.len(), 2, "{d:?}");
    assert_eq!((d[0].code, d[0].severity), ("QF047", Severity::Error));
    assert_eq!((d[1].code, d[1].severity), ("QF047", Severity::Warning));
}

fn pick(name: &str, keys: &[(&str, TypeRef)], spread: bool) -> Param {
    let keys: Vec<PickKeyMeta> = keys
        .iter()
        .enumerate()
        .map(|(i, (k, t))| PickKeyMeta {
            name: (*k).into(),
            required: false,
            db_index: i as u32 + 1,
            db_type: t.clone(),
        })
        .collect();
    Param {
        db_type: ty("record"),
        transform: if spread {
            Transform::PickSpread { keys }
        } else {
            Transform::Pick { keys }
        },
        ..param(name, ty("record"))
    }
}

#[test]
fn pick_key_type_change_missing_added_error() {
    let m = meta(
        vec![pick(
            "row",
            &[
                ("name", ty("text")),
                ("age", ty("int4")),
                ("gone", ty("int4")),
            ],
            false,
        )],
        vec![],
    );
    let actual = vec![pick(
        "row",
        &[
            ("name", ty("text")),
            ("age", ty("int8")),
            ("extra", ty("bool")),
        ],
        false,
    )];
    let d = compare(&m, &actual, &[]);
    let msgs: Vec<&str> = d.iter().map(|x| x.message.as_str()).collect();
    assert_eq!(
        msgs,
        [
            "getInvoice: parameter :row.age int4 → int8",
            "getInvoice: parameter :row.gone removed",
            "getInvoice: parameter :row.extra bool added",
        ]
    );
    assert!(
        d.iter()
            .all(|x| x.code == "QF045" && x.severity == Severity::Error)
    );
}

#[test]
fn pick_spread_keys_compared_identical_is_empty() {
    let keys = [("name", ty("text")), ("age", ty("int4"))];
    let m = meta(vec![pick("rows", &keys, true)], vec![]);
    assert_eq!(compare(&m, &[pick("rows", &keys, true)], &[]), vec![]);
    let changed = [("name", ty("varchar")), ("age", ty("int4"))];
    let d = compare(&m, &[pick("rows", &changed, true)], &[]);
    assert_eq!(d.len(), 1);
    assert_eq!(
        d[0].message,
        "getInvoice: parameter :rows.name text → varchar"
    );
}
