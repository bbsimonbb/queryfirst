//! Type-checks generated files with `tsc --strict`. Skipped (with a notice) only when `node` is
//! not installed.

use qf_gen_ts::{PgtypedBinding, TsOptions, generate};
use qf_meta::*;
use std::path::Path;
use std::process::Command;

const STUBS: &str = r#"declare module '@pgtyped/runtime' {
  export class PreparedQuery<TParamType, TResultType> {
    constructor(ir: any);
    run(params: TParamType): Promise<TResultType[]>;
  }
}
declare module 'queryfirst-runtime' {
  import { PreparedQuery } from '@pgtyped/runtime';
  export function one<P, R>(q: PreparedQuery<P, R>, name: string): (params: P) => Promise<R>;
  export function maybe<P, R>(q: PreparedQuery<P, R>, name: string): (params: P) => Promise<R | undefined>;
  export function none<P, R>(q: PreparedQuery<P, R>, name: string): (params: P) => Promise<void>;
}
"#;

fn address_meta() -> QueryMeta {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../qf-meta/tests/fixtures/address.qf.json");
    from_json(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn have_node() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn npx() -> &'static str {
    if cfg!(windows) { "npx.cmd" } else { "npx" }
}

/// Runs tsc over `files` in `dir`; returns (success, combined output).
fn tsc(dir: &Path, files: &[&str]) -> (bool, String) {
    let out = Command::new(npx())
        .current_dir(dir)
        .args([
            "-y",
            "-p",
            "typescript@5",
            "tsc",
            "--noEmit",
            "--strict",
            "--target",
            "es2020",
            "--module",
            "esnext",
            "--moduleResolution",
            "bundler",
            "--skipLibCheck",
            "--pretty",
            "false",
            "stubs.d.ts",
        ])
        .args(files)
        .output()
        .expect("run npx");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

#[test]
fn generated_files_type_check() {
    if !have_node() {
        println!("node not found: skipping the tsc check");
        return;
    }
    let dir = std::env::temp_dir().join(format!("qf-gen-ts-tsc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("stubs.d.ts"), STUBS).unwrap();
    let opts = TsOptions::default();

    // Fixtures that must compile.
    let good = address_meta();
    let mut one = good.clone();
    one.returns = Some(Returns::One);
    one.source = "one.psql".into();
    let mut described = good.clone();
    described.description = Some("A described query.".into());
    described.source = "described.psql".into();
    for (file, m) in [
        ("address.query.ts", &good),
        ("one.query.ts", &one),
        ("described.query.ts", &described),
    ] {
        let out = generate(m, None, &opts, &PgtypedBinding).unwrap();
        std::fs::write(dir.join(file), out.content).unwrap();
    }
    let (ok, text) = tsc(
        &dir,
        &["address.query.ts", "one.query.ts", "described.query.ts"],
    );
    println!("tsc (fixtures): ok={ok}\n{text}");
    assert!(ok, "{text}");

    // A broken query keeps the last good types and fails with exactly one error.
    let mut b = good.clone();
    b.status = Status::Broken {
        diagnostics: vec![MetaDiagnostic {
            code: "QF020".into(),
            severity: Severity::Error,
            message: "syntax error at \"FORM\"".into(),
            hint: None,
            line: 1,
            col: 1,
        }],
    };
    b.params.clear();
    b.columns.clear();
    let out = generate(&b, Some(&good), &opts, &PgtypedBinding).unwrap();
    std::fs::write(dir.join("broken.query.ts"), out.content).unwrap();
    let (ok, text) = tsc(&dir, &["broken.query.ts"]);
    println!("tsc (broken): ok={ok}\n{text}");
    assert!(!ok, "{text}");
    let errors: Vec<&str> = text.lines().filter(|l| l.contains("error TS")).collect();
    assert_eq!(errors.len(), 1, "{text}");
    assert!(errors[0].starts_with("broken.query.ts("), "{text}");
    assert!(errors[0].contains("syntax error at"), "{text}");
    assert!(errors[0].contains("addressBelongsToAccount"), "{text}");

    let _ = std::fs::remove_dir_all(&dir);
}
