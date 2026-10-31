//! Compares `pgtyped_ir` with the IRs pgtyped committed in a real corpus.
//! Gated on `QF_PGTYPED_CORPUS` (path to simplyfirst-app).

use qf_sqlfile::*;
use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn psql_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy();
        if p.is_dir() {
            if !matches!(&*name, "node_modules" | ".git" | ".next" | "dist" | "build") {
                psql_files(&p, out);
            }
        } else if name.ends_with(".psql") {
            out.push(p);
        }
    }
}

/// Test-only: split a multi-query pgtyped file into one segment per `/* @name` block.
fn split_queries(src: &str) -> Vec<&str> {
    let starts: Vec<usize> = src.match_indices("/* @name").map(|(i, _)| i).collect();
    starts
        .iter()
        .enumerate()
        .map(|(k, &a)| &src[a..starts.get(k + 1).copied().unwrap_or(src.len())])
        .collect()
}

#[test]
fn corpus_irs_match() {
    let Ok(root) = std::env::var("QF_PGTYPED_CORPUS") else {
        println!("QF_PGTYPED_CORPUS not set: skipping the pgtyped corpus test");
        return;
    };
    let root = PathBuf::from(root);
    assert!(
        root.is_dir(),
        "QF_PGTYPED_CORPUS={} does not exist",
        root.display()
    );

    let mut files = Vec::new();
    psql_files(&root, &mut files);
    files.sort();
    let re = Regex::new(r"const (\w+)IR: any = (.*);").unwrap();

    let (mut compared, mut excluded, mut no_sibling, mut multi, mut reformatted) = (0, 0, 0, 0, 0);
    let mut mismatches: Vec<String> = Vec::new();
    for f in &files {
        let ts = f.with_file_name(
            f.file_name()
                .unwrap()
                .to_string_lossy()
                .replace(".psql", ".queries.ts"),
        );
        let Ok(ts_src) = std::fs::read_to_string(&ts) else {
            no_sibling += 1;
            continue;
        };
        let ts_src = normalise(&ts_src);
        let irs: HashMap<String, String> = re
            .captures_iter(&ts_src)
            .map(|c| (c[1].to_string(), c[2].trim_end_matches('\r').to_string()))
            .collect();
        let src = normalise(&std::fs::read_to_string(f).unwrap());
        let segments = split_queries(&src);
        if segments.len() > 1 {
            multi += 1;
        }
        for seg in segments {
            let q = match parse_query_file(f, seg) {
                Ok(q) => q,
                Err(e) => {
                    mismatches.push(format!("{}: parse error {e:?}", f.display()));
                    continue;
                }
            };
            let Some(expected) = irs.get(&q.name) else {
                // Reformatted (e.g. by prettier) into a multi-line JS literal: not pgtyped's bytes.
                if ts_src.contains(&format!("const {}IR: any = {{\n", q.name)) {
                    reformatted += 1;
                    continue;
                }
                mismatches.push(format!("{}: no IR for {}", f.display(), q.name));
                continue;
            };
            let exp: serde_json::Value = serde_json::from_str(expected).unwrap();
            if exp["statement"].as_str().unwrap().contains('\r') {
                excluded += 1;
                continue;
            }
            let req: HashMap<String, bool> = exp["params"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| {
                    let r = p["required"].as_bool().unwrap_or(false);
                    (p["name"].as_str().unwrap().to_string(), r)
                })
                .collect();
            let lexed = match lex_params(&q) {
                Ok(l) => l,
                Err(e) => {
                    mismatches.push(format!("{} {}: lex error {e:?}", f.display(), q.name));
                    continue;
                }
            };
            let got = serde_json::to_string(&pgtyped_ir(&q, &lexed, &|n| {
                req.get(n).copied().unwrap_or(false)
            }))
            .unwrap();
            compared += 1;
            if &got != expected {
                mismatches.push(format!(
                    "{} {}:\n  expected {expected}\n  got      {got}",
                    f.display(),
                    q.name
                ));
            }
        }
    }
    println!(
        "pgtyped corpus: {} .psql files ({no_sibling} without .queries.ts, {multi} multi-query), \
         {compared} queries compared, {excluded} excluded (\\r in statement), \
         {reformatted} skipped (IR reformatted as multi-line JS), {} mismatches",
        files.len(),
        mismatches.len()
    );
    for m in mismatches.iter().take(20) {
        println!("{m}");
    }
    assert!(compared > 0, "no queries compared");
    assert!(mismatches.is_empty(), "{} mismatches", mismatches.len());
}
