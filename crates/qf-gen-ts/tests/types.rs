use qf_gen_ts::{
    GenDiagnostic, TsOptions, TypeAllocator, TypeOverrideOpt, locale_compare, render_types,
};
use qf_meta::*;
use std::cmp::Ordering;

fn tr(name: &str) -> TypeRef {
    TypeRef {
        schema: "pg_catalog".into(),
        name: name.into(),
        array: false,
        modifier: None,
        category: TypeCategory::Base,
        enum_values: None,
    }
}

fn param(name: &str, t: TypeRef, required: bool) -> Param {
    Param {
        name: name.into(),
        db_index: vec![1],
        db_type: t,
        required,
        required_reason: RequiredReason::DefaultNullable,
        transform: Transform::Scalar,
        locs: vec![],
        type_override: None,
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

fn fixture() -> QueryMeta {
    let json = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../qf-meta/tests/fixtures/address.qf.json"
    ))
    .unwrap();
    from_json(&json).expect("fixture")
}

fn meta(params: Vec<Param>, columns: Vec<Column>) -> QueryMeta {
    let mut m = fixture();
    m.name = "someQuery".into();
    m.params = params;
    m.columns = columns;
    m
}

fn render(m: &QueryMeta, o: &TsOptions) -> (String, TypeAllocator) {
    let mut a = TypeAllocator::default();
    let r = render_types(m, o, &mut a).expect("render");
    (
        format!("{}{}{}", r.params_iface, r.result_iface, r.query_iface),
        a,
    )
}

const EXPECTED_ADDRESS: &str = "/** 'AddressBelongsToAccount' parameters type */
export interface IAddressBelongsToAccountParams {
  accountId?: number | null | void;
  addressId?: number | null | void;
}

/** 'AddressBelongsToAccount' return type */
export interface IAddressBelongsToAccountResult {
  belongs: boolean | null;
}

/** 'AddressBelongsToAccount' query type */
export interface IAddressBelongsToAccountQuery {
  params: IAddressBelongsToAccountParams;
  result: IAddressBelongsToAccountResult;
}

";

#[test]
fn address_types_exact() {
    let (out, a) = render(&fixture(), &TsOptions::default());
    assert_eq!(out, EXPECTED_ADDRESS);
    assert_eq!(a.declarations(), "");
    // Also compare with the real pgtyped output when the sibling repo is present.
    if let Ok(s) = std::fs::read_to_string(
        "c:/dev/simplyfirst-app/app/api/(shared-queries)/address.queries.ts",
    ) {
        let s = s.replace("\r\n", "\n");
        let start = s.find("/** 'AddressBelongsToAccount' parameters").unwrap();
        let end = s.find("const addressBelongsToAccountIR").unwrap();
        assert_eq!(out, &s[start..end]);
    }
}

#[test]
fn field_order_locale_compare() {
    // node: [...].sort((a, b) => a.localeCompare(b))
    let mut v = vec!["id2", "Address", "address_line", "accountId", "_x", "id"];
    v.sort_by(|a, b| locale_compare(a, b));
    assert_eq!(
        v,
        ["_x", "accountId", "Address", "address_line", "id", "id2"]
    );

    let mut w = vec![
        "id2",
        "Address",
        "address_line",
        "accountId",
        "_x",
        "id",
        "a$b",
        "a1",
        "A",
        "a",
        "B",
        "b",
        "a_b",
        "ab",
        "a-b",
        "id10",
        "id9",
    ];
    w.sort_by(|a, b| locale_compare(a, b));
    assert_eq!(
        w,
        [
            "_x",
            "a",
            "A",
            "a_b",
            "a-b",
            "a$b",
            "a1",
            "ab",
            "accountId",
            "Address",
            "address_line",
            "b",
            "B",
            "id",
            "id10",
            "id2",
            "id9"
        ]
    );
    assert_eq!(locale_compare("a", "a"), Ordering::Equal);
}

#[test]
fn types_overrides_numeric_number() {
    let m = meta(
        vec![param("p", tr("numeric"), true)],
        vec![col("c", tr("numeric"), false)],
    );
    let (out, a) = render(&m, &TsOptions::default());
    assert!(out.contains("  p: NumberOrString;"));
    assert!(out.contains("  c: string;"));
    assert_eq!(
        a.declarations(),
        "export type NumberOrString = number | string;\n"
    );
    let mut o = TsOptions::default();
    o.types_overrides
        .insert("numeric".into(), TypeOverrideOpt::Both("number".into()));
    let (out, a) = render(&m, &o);
    assert!(out.contains("  p: number;"));
    assert!(out.contains("  c: number;"));
    assert_eq!(a.declarations(), "");
    let mut o = TsOptions::default();
    o.types_overrides.insert(
        "numeric".into(),
        TypeOverrideOpt::Split {
            parameter: Some("number".into()),
            ret: None,
        },
    );
    let (out, _) = render(&m, &o);
    assert!(out.contains("  p: number;"));
    assert!(out.contains("  c: string;"));
}

#[test]
fn camel_case_columns() {
    let m = meta(
        vec![],
        vec![
            col("address_line_1", tr("text"), false),
            col("id", tr("int4"), false),
        ],
    );
    let o = TsOptions {
        camel_case_column_names: true,
        ..Default::default()
    };
    let (out, _) = render(&m, &o);
    // camel-case keeps a `_` before a digit-leading word
    assert!(out.contains("  addressLine_1: string;"), "{out}");
    let (out, _) = render(&m, &TsOptions::default());
    assert!(out.contains("  address_line_1: string;"));
}

#[test]
fn enum_union_sorted() {
    let e = TypeRef {
        schema: "public".into(),
        name: "accessLevel".into(),
        array: false,
        modifier: None,
        category: TypeCategory::Enum,
        enum_values: Some(vec!["read".into(), "edit".into(), "none".into()]),
    };
    let m = meta(
        vec![param("level", e.clone(), true)],
        vec![col("level", e, true)],
    );
    let (out, a) = render(&m, &TsOptions::default());
    assert!(out.contains("  level: accessLevel;"));
    assert!(out.contains("  level: accessLevel | null;"));
    assert_eq!(
        a.declarations(),
        "export type accessLevel = 'edit' | 'none' | 'read';\n"
    );
}

#[test]
fn array_type_alias() {
    let mut t = tr("text");
    t.array = true;
    let m = meta(
        vec![param("tags", t.clone(), true)],
        vec![col("tags", t, false)],
    );
    let (out, a) = render(&m, &TsOptions::default());
    assert!(out.contains("  tags: stringArray;"));
    assert_eq!(a.declarations(), "export type stringArray = (string)[];\n");
}

#[test]
fn type_override_import_and_nullable() {
    let mut c = col("theme", tr("jsonb"), true);
    c.type_override = Some(TypeOverride {
        name: "Themes".into(),
        module: Some("@/types".into()),
        array: false,
    });
    let m = meta(vec![], vec![c]);
    let (out, a) = render(&m, &TsOptions::default());
    assert!(out.contains("  theme: Themes | null;"));
    assert_eq!(a.declarations(), "import type { Themes } from '@/types';\n");
}

#[test]
fn type_override_name_collision_aliases() {
    let mk = |n: &str, module: &str| {
        let mut c = col(n, tr("jsonb"), false);
        c.type_override = Some(TypeOverride {
            name: "Themes".into(),
            module: Some(module.into()),
            array: false,
        });
        c
    };
    let m = meta(vec![], vec![mk("a", "m1"), mk("b", "m2")]);
    let (out, a) = render(&m, &TsOptions::default());
    assert!(out.contains("  a: Themes;"));
    assert!(out.contains("  b: Themes_1;"));
    assert_eq!(
        a.declarations(),
        "import type { Themes } from 'm1';\n\nimport type { Themes as Themes_1 } from 'm2';\n"
    );
}

#[test]
fn pick_and_spread() {
    let mut s = param("ids", tr("int4"), true);
    s.transform = Transform::Spread;
    let mut p = param("row", tr("int4"), true);
    p.transform = Transform::PickSpread {
        keys: vec![
            PickKeyMeta {
                name: "a".into(),
                required: true,
                db_index: 1,
                db_type: tr("int4"),
            },
            PickKeyMeta {
                name: "b".into(),
                required: false,
                db_index: 2,
                db_type: tr("text"),
            },
        ],
    };
    let m = meta(vec![s, p], vec![]);
    let (out, _) = render(&m, &TsOptions::default());
    assert!(out.contains("  ids: readonly (number)[];"));
    assert!(
        out.contains("  row: readonly ({\n    a: number,\n    b: string | null | void\n  })[];")
    );
}

#[test]
fn void_result_and_void_params() {
    let m = meta(vec![], vec![]);
    let (out, _) = render(&m, &TsOptions::default());
    assert!(
        out.contains(
            "/** 'SomeQuery' parameters type */\nexport type ISomeQueryParams = void;\n\n"
        )
    );
    assert!(
        out.contains("/** 'SomeQuery' return type */\nexport type ISomeQueryResult = void;\n\n")
    );
}

#[test]
fn unsupported_type_qf014() {
    let m = meta(vec![param("p", tr("geometry"), true)], vec![]);
    let mut a = TypeAllocator::default();
    let e: Vec<GenDiagnostic> = render_types(&m, &TsOptions::default(), &mut a).unwrap_err();
    assert_eq!(e[0].code, "QF014");
    assert!(e[0].message.contains("geometry"));
}

#[test]
fn array_override_renders_name_brackets() {
    let mk = |nullable: bool| {
        let mut c = col("v", tr("_text"), nullable);
        c.type_override = Some(TypeOverride {
            name: "VatCountries".into(),
            module: Some("@/types/vat".into()),
            array: true,
        });
        c
    };
    let m = meta(vec![], vec![mk(false)]);
    let (out, a) = render(&m, &TsOptions::default());
    assert!(out.contains("  v: VatCountries[];"), "{out}");
    assert_eq!(
        a.declarations(),
        "import type { VatCountries } from '@/types/vat';\n"
    );
    let m = meta(vec![], vec![mk(true)]);
    let (out, _) = render(&m, &TsOptions::default());
    assert!(out.contains("  v: VatCountries[] | null;"), "{out}");
}

#[test]
fn field_order_end_to_end() {
    let names = ["id2", "Address", "address_line", "accountId", "_x", "id"];
    let m = meta(
        vec![],
        names.iter().map(|n| col(n, tr("int4"), false)).collect(),
    );
    let (out, _) = render(&m, &TsOptions::default());
    let order: Vec<&str> = out
        .split("Result {\n")
        .nth(1)
        .unwrap()
        .lines()
        .take_while(|l| *l != "}")
        .map(|l| l.trim().split(':').next().unwrap())
        .collect();
    assert_eq!(
        order,
        ["_x", "accountId", "Address", "address_line", "id", "id2"]
    );
}
