//! Type rendering: port of pgtyped's `TypeAllocator` and `queryToTypeDeclarations`.

use crate::collate::locale_compare;
use crate::mapping::{TsType, default_mapping, get_array};
use qf_meta::{
    Column, Param, PickKeyMeta, QueryMeta, Transform, TypeCategory, TypeOverride, TypeRef,
};
use std::collections::BTreeMap;

/// pgtyped `typesOverrides` config shapes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeOverrideOpt {
    Both(String),
    Split {
        parameter: Option<String>,
        ret: Option<String>,
    },
}

#[derive(Debug, Clone, Default)]
pub struct TsOptions {
    pub types_overrides: BTreeMap<String, TypeOverrideOpt>,
    pub camel_case_column_names: bool,
    pub emit_template: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenDiagnostic {
    pub code: &'static str,
    pub message: String,
}

/// Each interface is preceded by its `/** 'Name' ... type */` comment and followed by a blank line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedTypes {
    pub params_iface: String,
    pub result_iface: String,
    pub query_iface: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Parameter,
    Return,
}

/// Tracks used types, imports and helper aliases; mirrors pgtyped's `TypeAllocator`.
#[derive(Debug, Clone, Default)]
pub struct TypeAllocator {
    errors: Vec<GenDiagnostic>,
    /// from -> imported types, in first-use order.
    imports: Vec<(String, Vec<TsType>)>,
    /// First-use order, unique by name.
    types: Vec<TsType>,
}

/// pgtyped `stringToType`.
fn string_to_type(s: &str) -> TsType {
    if s.starts_with("./") || s.starts_with("../") || s.contains('#') || s.contains(" as ") {
        let (first, alias) = match s.split_once(" as ") {
            Some((f, a)) => (f, Some(a)),
            None => (s, None),
        };
        let (from, named) = match first.split_once('#') {
            Some((f, n)) => (f, Some(n)),
            None => (first, None),
        };
        let name = alias.or(named).unwrap_or(first);
        return TsType::Import {
            name: name.into(),
            from: from.into(),
            alias_of: alias.map(|_| named.unwrap_or("default").to_string()),
        };
    }
    TsType::Plain(s.into())
}

/// pgtyped `declareImport`, except that relative module paths are emitted verbatim.
fn declare_import(imports: &[TsType]) -> String {
    let mut names: Vec<(String, String)> = Vec::new(); // (imported, local)
    let mut default_alias: Option<String> = None;
    let mut from = String::new();
    for imp in imports {
        let TsType::Import {
            name,
            from: f,
            alias_of,
        } = imp
        else {
            continue;
        };
        if from.is_empty() {
            from = f.clone();
        }
        if alias_of.as_deref() == Some("default") {
            default_alias.get_or_insert_with(|| name.clone());
            continue;
        }
        let imported = alias_of.clone().unwrap_or_else(|| name.clone());
        if !names.iter().any(|(i, _)| *i == imported) {
            names.push((imported, name.clone()));
        }
    }
    let mut lines = Vec::new();
    if let Some(d) = default_alias {
        let dec = format!("import type {d} from '{from}';");
        if names.is_empty() {
            return format!("{dec}\n");
        }
        lines.push(dec);
    }
    let kw = if from == "@pgtyped/runtime" {
        "import"
    } else {
        "import type"
    };
    let list = names
        .iter()
        .map(|(i, l)| {
            if i == l {
                i.clone()
            } else {
                format!("{i} as {l}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    lines.push(format!("{kw} {{ {list} }} from '{from}';\n"));
    lines.join("\n")
}

impl TypeAllocator {
    /// Imports, then enums (sorted), then aliases (sorted), as `typeDefinitionDeclarations`.
    pub fn declarations(&self) -> String {
        let imports = self
            .imports
            .iter()
            .map(|(_, v)| declare_import(v))
            .collect::<Vec<_>>()
            .join("\n");
        let mut enums: Vec<String> = Vec::new();
        let mut aliases: Vec<String> = Vec::new();
        for t in &self.types {
            match t {
                TsType::Enum { name, values } => {
                    let mut v = values.clone();
                    v.sort();
                    let u = v
                        .iter()
                        .map(|x| format!("'{x}'"))
                        .collect::<Vec<_>>()
                        .join(" | ");
                    enums.push(format!("export type {name} = {u};\n"));
                }
                TsType::Alias { name, definition } => {
                    aliases.push(format!("export type {name} = {definition};\n"));
                }
                _ => {}
            }
        }
        enums.sort();
        aliases.sort();
        [imports, enums.join("\n"), aliases.join("\n")]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn register(&mut self, typ: &TsType) -> String {
        if !self.types.iter().any(|t| t.name() == typ.name()) {
            self.types.push(typ.clone());
        }
        if let TsType::Import { from, .. } = typ {
            let idx = match self.imports.iter().position(|(f, _)| f == from) {
                Some(i) => i,
                None => {
                    self.imports.push((from.clone(), Vec::new()));
                    self.imports.len() - 1
                }
            };
            if !self.imports[idx].1.contains(typ) {
                self.imports[idx].1.push(typ.clone());
            }
        }
        typ.name().to_string()
    }

    /// Import `name` from `module`, aliasing as `name_N` if another module already owns `name`.
    fn import_named(&self, name: &str, module: &str) -> TsType {
        let mut n = 0;
        loop {
            let local = if n == 0 {
                name.to_string()
            } else {
                format!("{name}_{n}")
            };
            let existing = self
                .imports
                .iter()
                .flat_map(|(_, v)| v.iter())
                .find(|t| t.name() == local);
            match existing {
                None => {
                    return TsType::Import {
                        name: local,
                        from: module.into(),
                        alias_of: (n != 0).then(|| name.to_string()),
                    };
                }
                Some(t @ TsType::Import { from, alias_of, .. })
                    if from == module && alias_of.as_deref().unwrap_or(&local) == name =>
                {
                    return t.clone();
                }
                _ => n += 1,
            }
        }
    }

    fn error(&mut self, code: &'static str, message: String) {
        self.errors.push(GenDiagnostic { code, message });
    }

    fn lookup(&self, name: &str, scope: Scope, opts: &TsOptions) -> Option<TsType> {
        let over = opts
            .types_overrides
            .get(name)
            .and_then(|o| match (o, scope) {
                (TypeOverrideOpt::Both(s), _) => Some(s),
                (TypeOverrideOpt::Split { parameter, .. }, Scope::Parameter) => parameter.as_ref(),
                (TypeOverrideOpt::Split { ret, .. }, Scope::Return) => ret.as_ref(),
            });
        if let Some(s) = over {
            return Some(string_to_type(s));
        }
        default_mapping(name).map(|(p, r)| match scope {
            Scope::Parameter => p,
            Scope::Return => r,
        })
    }

    /// pgtyped `use` for a database type name (`_` prefix means array).
    fn use_name(&mut self, name: &str, scope: Scope, opts: &TsOptions) -> String {
        let typ = if let Some(elem) = name.strip_prefix('_') {
            let mapped = self.use_name(elem, scope, opts);
            get_array(&mapped, None)
        } else {
            match self.lookup(name, scope, opts) {
                Some(t) => t,
                None => {
                    self.error(
                        "QF014",
                        format!("Postgres type '{name}' is not supported by mapping"),
                    );
                    return "unknown".into();
                }
            }
        };
        self.register(&typ)
    }

    fn use_ref(&mut self, tr: &TypeRef, scope: Scope, opts: &TsOptions) -> String {
        let values = match (tr.category, &tr.enum_values) {
            (TypeCategory::Enum, Some(v)) => v.clone(),
            _ => {
                let n = if tr.array {
                    format!("_{}", tr.name)
                } else {
                    tr.name.clone()
                };
                return self.use_name(&n, scope, opts);
            }
        };
        let enum_type = TsType::Enum {
            name: tr.name.clone(),
            values,
        };
        let mapped = self.lookup(&tr.name, scope, opts);
        if !tr.array {
            return self.register(&mapped.unwrap_or(enum_type));
        }
        let arr = get_array(&tr.name, mapped.as_ref().map(|m| m.name()));
        // make sure the element type is used so it appears in the declaration
        self.register(&mapped.unwrap_or(enum_type));
        self.register(&arr)
    }

    fn use_override(&mut self, o: &TypeOverride) -> String {
        let local = match &o.module {
            Some(m) => {
                let t = self.import_named(&o.name, m);
                self.register(&t)
            }
            None => o.name.clone(),
        };
        if o.array { format!("{local}[]") } else { local }
    }

    fn type_of(
        &mut self,
        tr: &TypeRef,
        ov: Option<&TypeOverride>,
        scope: Scope,
        opts: &TsOptions,
    ) -> String {
        match ov {
            Some(o) => self.use_override(o),
            None => self.use_ref(tr, scope, opts),
        }
    }
}

// ---- casing helpers (ports of no-case / pascal-case / camel-case) ----

fn words(input: &str) -> Vec<String> {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::new();
    // Separate at lower|digit -> Upper and Upper -> Upper lower boundaries.
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 {
            let p = chars[i - 1];
            let split1 = (p.is_ascii_lowercase() || p.is_ascii_digit()) && c.is_ascii_uppercase();
            let split2 = p.is_ascii_uppercase()
                && c.is_ascii_uppercase()
                && chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            if split1 || split2 {
                out.push('\0');
            }
        }
        out.push(c);
    }
    out.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect()
}

fn pascal_word(w: &str, index: usize) -> String {
    let lower = w.to_lowercase();
    let mut cs = lower.chars();
    let first = cs.next().unwrap_or(' ');
    if index > 0 && first.is_ascii_digit() {
        return format!("_{lower}");
    }
    format!("{}{}", first.to_ascii_uppercase(), cs.as_str())
}

fn pascal_case(s: &str) -> String {
    words(s)
        .iter()
        .enumerate()
        .map(|(i, w)| pascal_word(w, i))
        .collect()
}

fn camel_case(s: &str) -> String {
    words(s)
        .iter()
        .enumerate()
        .map(|(i, w)| {
            if i == 0 {
                w.to_lowercase()
            } else {
                pascal_word(w, i)
            }
        })
        .collect()
}

fn escape_key(k: &str) -> String {
    let mut cs = k.chars();
    let ok = cs
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if ok {
        k.to_string()
    } else {
        format!("\"{k}\"")
    }
}

struct Field {
    optional: bool,
    name: String,
    ty: String,
}

fn interface(name: &str, mut fields: Vec<Field>) -> String {
    fields.sort_by(|a, b| locale_compare(&a.name, &b.name));
    let body = fields
        .iter()
        .map(|f| {
            format!(
                "  {}{}: {};",
                escape_key(&f.name),
                if f.optional { "?" } else { "" },
                f.ty
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("export interface {name} {{\n{body}\n}}\n\n")
}

fn type_alias(name: &str, alias: &str) -> String {
    format!("export type {name} = {alias};\n\n")
}

fn column_field(c: &Column, a: &mut TypeAllocator, opts: &TsOptions) -> Field {
    let mut ty = a.type_of(&c.db_type, c.type_override.as_ref(), Scope::Return, opts);
    if c.nullable {
        ty.push_str(" | null");
    }
    Field {
        optional: false,
        name: if opts.camel_case_column_names {
            camel_case(&c.name)
        } else {
            c.name.clone()
        },
        ty,
    }
}

fn pick_object(keys: &[PickKeyMeta], a: &mut TypeAllocator, opts: &TsOptions) -> String {
    let body = keys
        .iter()
        .map(|k| {
            let t = a.use_ref(&k.db_type, Scope::Parameter, opts);
            if k.required {
                format!("    {}: {}", k.name, t)
            } else {
                format!("    {}: {} | null | void", k.name, t)
            }
        })
        .collect::<Vec<_>>()
        .join(",\n");
    format!("{{\n{body}\n  }}")
}

fn param_field(p: &Param, a: &mut TypeAllocator, opts: &TsOptions) -> Field {
    match &p.transform {
        Transform::Scalar | Transform::Spread => {
            let mut ty = a.type_of(&p.db_type, p.type_override.as_ref(), Scope::Parameter, opts);
            if !p.required {
                ty.push_str(" | null | void");
            }
            let spread = p.transform == Transform::Spread;
            Field {
                optional: !spread && !p.required,
                name: p.name.clone(),
                ty: if spread {
                    format!("readonly ({ty})[]")
                } else {
                    ty
                },
            }
        }
        Transform::Pick { keys } | Transform::PickSpread { keys } => {
            let obj = pick_object(keys, a, opts);
            let spread = matches!(p.transform, Transform::PickSpread { .. });
            Field {
                optional: false,
                name: p.name.clone(),
                ty: if spread {
                    format!("readonly ({obj})[]")
                } else {
                    obj
                },
            }
        }
    }
}

/// Render the three interfaces for a query. Unsupported types yield `QF014` diagnostics.
pub fn render_types(
    meta: &QueryMeta,
    opts: &TsOptions,
    alloc: &mut TypeAllocator,
) -> Result<RenderedTypes, Vec<GenDiagnostic>> {
    let errors_before = alloc.errors.len();
    let query_name = pascal_case(&meta.name);
    let params_name = format!("I{query_name}Params");
    let result_name = format!("I{query_name}Result");
    let query_iface_name = format!("I{query_name}Query");

    let result_fields: Vec<Field> = meta
        .columns
        .iter()
        .map(|c| column_field(c, alloc, opts))
        .collect();
    let param_fields: Vec<Field> = meta
        .params
        .iter()
        .map(|p| param_field(p, alloc, opts))
        .collect();

    if alloc.errors.len() > errors_before {
        return Err(alloc.errors[errors_before..].to_vec());
    }

    let result_iface = format!(
        "/** '{query_name}' return type */\n{}",
        if result_fields.is_empty() {
            type_alias(&result_name, "void")
        } else {
            interface(&result_name, result_fields)
        }
    );
    let params_iface = format!(
        "/** '{query_name}' parameters type */\n{}",
        if param_fields.is_empty() {
            type_alias(&params_name, "void")
        } else {
            interface(&params_name, param_fields)
        }
    );
    let query_iface = format!(
        "/** '{query_name}' query type */\n{}",
        interface(
            &query_iface_name,
            vec![
                Field {
                    optional: false,
                    name: "params".into(),
                    ty: params_name,
                },
                Field {
                    optional: false,
                    name: "result".into(),
                    ty: result_name,
                },
            ],
        )
    );
    Ok(RenderedTypes {
        params_iface,
        result_iface,
        query_iface,
    })
}
