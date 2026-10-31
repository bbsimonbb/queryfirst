use qf_gen_ts::{PgtypedBinding, TsOptions, generate, meta_ir, output_path};
use qf_meta::*;
use std::path::{Path, PathBuf};

const GOLDEN: &str = include_str!("fixtures/address.query.ts");

fn address_meta() -> QueryMeta {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../qf-meta/tests/fixtures/address.qf.json");
    from_json(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn opts() -> TsOptions {
    TsOptions::default()
}

fn gen_ok(m: &QueryMeta) -> String {
    generate(m, None, &opts(), &PgtypedBinding).unwrap().content
}

fn broken(m: &QueryMeta, msg: &str) -> QueryMeta {
    let mut b = m.clone();
    b.status = Status::Broken {
        diagnostics: vec![MetaDiagnostic {
            code: "QF020".into(),
            severity: Severity::Error,
            message: msg.into(),
            hint: None,
            line: 1,
            col: 1,
        }],
    };
    b
}

fn tr() -> TypeRef {
    TypeRef {
        schema: "pg_catalog".into(),
        name: "int4".into(),
        array: false,
        modifier: None,
        category: TypeCategory::Base,
        enum_values: None,
    }
}

#[test]
fn address_file_exact() {
    let out = generate(&address_meta(), None, &opts(), &PgtypedBinding).unwrap();
    assert_eq!(out.path, "app/api/(shared-queries)/address.query.ts");
    assert_eq!(out.content, GOLDEN);
    assert!(!out.content.contains('\r'));
    // The golden is the real pgtyped file, CRLF -> LF.
    let real = PathBuf::from("c:/dev/simplyfirst-app/app/api/(shared-queries)/address.queries.ts");
    if let Ok(s) = std::fs::read_to_string(real) {
        assert_eq!(out.content, s.replace("\r\n", "\n"));
    }
}

#[test]
fn ir_matches_sqlfile() {
    use qf_sqlfile::{ExpansionKind, lex_params, normalise, parse_query_file, pgtyped_ir};
    let sources = [
        "/* @name a */\nSELECT :b, :a, :b FROM t WHERE x = :a",
        "/* @name q\n @param ids -> (...)\n @param user -> (name, age!)\n @param rows -> ((a, b!)...)\n @param unused -> (...)\n*/\nSELECT :x, :rows, :user, :ids, 'é' /* :c */ ",
        "/* @name r */\nSELECT 1 -- :nope\n",
    ];
    for src in sources {
        let q = parse_query_file(Path::new("q.psql"), &normalise(src)).unwrap();
        let lexed = lex_params(&q).unwrap();
        let required = |n: &str| n.len() % 2 == 1;
        let expected = serde_json::to_string(&pgtyped_ir(&q, &lexed, &required)).unwrap();

        // Rebuild a meta in pgtyped's param order.
        let mut order: Vec<(String, Transform)> = Vec::new();
        let keys = |k: &[qf_sqlfile::PickKey]| -> Vec<PickKeyMeta> {
            k.iter()
                .map(|k| PickKeyMeta {
                    name: k.name.clone(),
                    required: k.required,
                    db_index: 1,
                    db_type: tr(),
                })
                .collect()
        };
        for e in &q.expansions {
            let t = match &e.kind {
                ExpansionKind::Spread => Transform::Spread,
                ExpansionKind::Pick(k) => Transform::Pick { keys: keys(k) },
                ExpansionKind::PickSpread(k) => Transform::PickSpread { keys: keys(k) },
            };
            order.push((e.param.clone(), t));
        }
        for u in &lexed.uses {
            if !order.iter().any(|(n, _)| *n == u.name) {
                order.push((u.name.clone(), Transform::Scalar));
            }
        }
        let mut m = address_meta();
        m.params = order
            .into_iter()
            .map(|(name, transform)| Param {
                locs: lexed
                    .uses
                    .iter()
                    .filter(|u| u.name == name)
                    .map(|u| Loc {
                        a: u.loc.a,
                        b: u.loc.b,
                    })
                    .collect(),
                required: required(&name),
                db_index: vec![1],
                db_type: tr(),
                required_reason: RequiredReason::DefaultNullable,
                type_override: None,
                name,
                transform,
            })
            .collect();
        m.statement = Statement {
            source: lexed.statement.clone(),
            db: lexed.db_statement.clone(),
        };
        assert_eq!(
            serde_json::to_string(&meta_ir(&m)).unwrap(),
            expected,
            "{src}"
        );
    }
}

const PLAIN: &str = "export const addressBelongsToAccount = new PreparedQuery<IAddressBelongsToAccountParams,IAddressBelongsToAccountResult>(addressBelongsToAccountIR);\n";

#[test]
fn returns_one_wraps() {
    let mut m = address_meta();
    m.returns = Some(Returns::One);
    let c = gen_ok(&m);
    assert!(
        c.starts_with(concat!(
            "/** Types generated for queries found in \"app/api/(shared-queries)/address.psql\" */\n",
            "import { PreparedQuery } from '@pgtyped/runtime';\n",
            "import { one } from 'queryfirst-runtime';\n\n"
        )),
        "{c}"
    );
    assert!(c.contains(concat!(
        "export const addressBelongsToAccount = one(new PreparedQuery<IAddressBelongsToAccountParams,",
        "IAddressBelongsToAccountResult>(addressBelongsToAccountIR), \"addressBelongsToAccount\");\n\n\n"
    )));
    m.returns = Some(Returns::Maybe);
    assert!(gen_ok(&m).contains("import { maybe } from 'queryfirst-runtime';"));
    m.returns = Some(Returns::None);
    assert!(gen_ok(&m).contains("= none(new PreparedQuery<"));
}

#[test]
fn returns_absent_plain() {
    let mut m = address_meta();
    for r in [None, Some(Returns::Many)] {
        m.returns = r;
        let c = gen_ok(&m);
        assert!(c.contains(PLAIN));
        assert!(!c.contains("queryfirst-runtime"));
        assert_eq!(c, GOLDEN);
    }
}

#[test]
fn description_in_doc_comment() {
    let mut m = address_meta();
    m.description = Some("Does this belong?\nSecond */ line".into());
    let c = gen_ok(&m);
    assert!(
        c.contains(
            "/**\n * Does this belong?\n * Second *\\/ line\n * Query generated from SQL:\n * ```\n"
        ),
        "{c}"
    );
}

#[test]
fn broken_keeps_last_good_types_and_adds_never() {
    let good = address_meta();
    let b = broken(&good, "syntax error at \"FORM\"\nline 2");
    let mut b = b;
    b.params.clear();
    b.columns.clear();
    let out = generate(&b, Some(&good), &opts(), &PgtypedBinding).unwrap();
    assert_eq!(
        out.content,
        format!(
            "{GOLDEN}export const queryfirstError: never = \"addressBelongsToAccount: syntax error at \\\"FORM\\\"\\nline 2\";\n"
        )
    );

    // A last good query with no params and no columns (a `DELETE`) still has its wrapper: the
    // meta's statement, not its params or columns, says there is a last good version.
    let mut void = address_meta();
    void.name = "deleteExpired".into();
    void.description = None;
    void.params.clear();
    void.columns.clear();
    void.statement = Statement {
        source: "DELETE FROM sessions WHERE expires < now()".into(),
        db: "DELETE FROM sessions WHERE expires < now()".into(),
    };
    let good = gen_ok(&void);
    assert!(
        good.contains("export type IDeleteExpiredParams = void;"),
        "{good}"
    );
    assert!(
        good.contains("export type IDeleteExpiredResult = void;"),
        "{good}"
    );
    let b = broken(&void, "boom");
    let c = generate(&b, None, &opts(), &PgtypedBinding)
        .unwrap()
        .content;
    assert_eq!(
        c,
        format!("{good}export const queryfirstError: never = \"deleteExpired: boom\";\n")
    );
}

#[test]
fn broken_uses_own_types_without_last_good() {
    let b = broken(&address_meta(), "boom");
    let c = generate(&b, None, &opts(), &PgtypedBinding)
        .unwrap()
        .content;
    assert!(c.contains("export interface IAddressBelongsToAccountParams"));
    assert!(
        c.ends_with("export const queryfirstError: never = \"addressBelongsToAccount: boom\";\n")
    );
}

#[test]
fn broken_without_last_good_is_error_only() {
    let mut b = broken(&address_meta(), "boom");
    b.params.clear();
    b.columns.clear();
    b.statement = Statement {
        source: String::new(),
        db: String::new(),
    };
    let c = generate(&b, None, &opts(), &PgtypedBinding)
        .unwrap()
        .content;
    assert_eq!(
        c,
        concat!(
            "/** Types generated for queries found in \"app/api/(shared-queries)/address.psql\" */\n",
            "export const queryfirstError: never = \"addressBelongsToAccount: boom\";\n"
        )
    );
}

#[test]
fn output_path_template_brackets() {
    let mut o = opts();
    assert_eq!(
        output_path("app/orders/[id]/(queries)/get.psql", &o),
        "app/orders/[id]/(queries)/get.query.ts"
    );
    assert_eq!(output_path("get.psql", &o), "get.query.ts");
    o.emit_template = "{{dir}}/{{name}}.queries.ts".into();
    assert_eq!(output_path("a/b/get.psql", &o), "a/b/get.queries.ts");
}
