//! File assembly, mirroring pgtyped's `generateDeclarationFile` and `generateDeclarations`.

use crate::binding::RuntimeBinding;
use crate::pgtyped_binding::meta_ir;
use crate::types::{GenDiagnostic, TsOptions, TypeAllocator, pascal_case, render_types};
use qf_meta::{QueryMeta, Status};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFile {
    /// Project-relative, POSIX.
    pub path: String,
    pub content: String,
}

const DEFAULT_TEMPLATE: &str = "{{dir}}/{{name}}.qf.ts";

/// Expand `emit_template` (`{{dir}}`, `{{name}}`) for a project-relative POSIX source path.
pub fn output_path(source: &str, opts: &TsOptions) -> String {
    let (dir, file) = match source.rsplit_once('/') {
        Some((d, f)) => (d, f),
        None => ("", source),
    };
    let name = file.rsplit_once('.').map_or(file, |(n, _)| n);
    let template = if opts.emit_template.is_empty() {
        DEFAULT_TEMPLATE
    } else {
        &opts.emit_template
    };
    let out = template.replace("{{dir}}", dir).replace("{{name}}", name);
    if dir.is_empty() {
        out.strip_prefix('/').unwrap_or(&out).to_string()
    } else {
        out
    }
}

/// Escape for a double-quoted JS string literal on one line.
fn js_string(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 || matches!(c, '\u{2028}' | '\u{2029}') => {
                o.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn header(source: &str) -> String {
    format!("/** Types generated for queries found in \"{source}\" */\n")
}

fn doc_comment(meta: &QueryMeta) -> String {
    let mut s = String::from("/**\n");
    if let Some(d) = &meta.description {
        for line in d.split('\n') {
            let line = line.trim_end_matches('\r').replace("*/", "*\\/");
            if line.is_empty() {
                s.push_str(" *\n");
            } else {
                s.push_str(&format!(" * {line}\n"));
            }
        }
    }
    s.push_str(" * Query generated from SQL:\n * ```\n");
    let body = meta
        .statement
        .source
        .split('\n')
        .map(|l| format!(" * {l}"))
        .collect::<Vec<_>>()
        .join("\n");
    s.push_str(&body);
    s.push_str("\n * ```\n */\n");
    s
}

/// The complete file for one query whose types come from `meta`.
fn render_file(
    meta: &QueryMeta,
    opts: &TsOptions,
    binding: &dyn RuntimeBinding,
) -> Result<String, Vec<GenDiagnostic>> {
    let mut alloc = TypeAllocator::default();
    alloc.register_runtime_import();
    let types = render_types(meta, opts, &mut alloc)?;

    // `declarations()` starts with the PreparedQuery import line.
    let decls = alloc.declarations();
    let (first, rest) = decls.split_once('\n').unwrap_or((&decls, ""));
    let mut out = header(&meta.source);
    out.push_str(first);
    out.push('\n');
    for imp in binding.imports(meta) {
        out.push_str(&imp);
        out.push('\n');
    }
    out.push_str(rest);
    out.push('\n');

    let pascal = pascal_case(&meta.name);
    out.push_str(&types.params_iface);
    out.push_str(&types.result_iface);
    out.push_str(&types.query_iface);
    out.push_str(&format!(
        "const {}IR: any = {};\n\n",
        meta.name,
        serde_json::to_string(&meta_ir(meta)).expect("IR serialises")
    ));
    out.push_str(&doc_comment(meta));
    out.push_str(&binding.query_export(
        meta,
        &format!("I{pascal}Params"),
        &format!("I{pascal}Result"),
    ));
    out.push_str("\n\n");
    Ok(out)
}

/// Generate the `.qf.ts` for `meta`. A broken query keeps the types of `last_good` (else its
/// own last good types, which it has when it has a statement) and gets a trailing
/// `queryfirstError: never` so that `tsc` fails.
pub fn generate(
    meta: &QueryMeta,
    last_good: Option<&QueryMeta>,
    opts: &TsOptions,
    binding: &dyn RuntimeBinding,
) -> Result<OutputFile, Vec<GenDiagnostic>> {
    let path = output_path(&meta.source, opts);
    let content = match &meta.status {
        Status::Ok => render_file(meta, opts, binding)?,
        Status::Broken { diagnostics } => {
            let basis = match last_good {
                Some(g) => {
                    // Last good params and columns; the current annotations and name.
                    let mut m = g.clone();
                    m.source = meta.source.clone();
                    m.name = meta.name.clone();
                    m.description = meta.description.clone();
                    m.returns = meta.returns;
                    Some(m)
                }
                // The meta keeps a last good statement (possibly with no params and no
                // columns, e.g. a `DELETE`) exactly when it has last good types.
                None if !meta.statement.source.is_empty() => Some(meta.clone()),
                None => None,
            };
            let mut out = match basis {
                Some(m) => render_file(&m, opts, binding)?,
                None => header(&meta.source),
            };
            let msg = diagnostics
                .first()
                .map_or("query is broken", |d| d.message.as_str());
            out.push_str(&format!(
                "export const queryfirstError: never = {};\n",
                js_string(&format!("{}: {msg}", meta.name))
            ));
            out
        }
    };
    Ok(OutputFile { path, content })
}
