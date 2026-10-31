//! `@pgtyped/runtime` binding: `PreparedQuery` plus the Queryfirst `@returns` wrappers.

use crate::binding::RuntimeBinding;
use qf_meta::{PickKeyMeta, QueryMeta, Returns, Transform};
use serde_json::{Map, Value, json};

pub struct PgtypedBinding;

fn wrapper(meta: &QueryMeta) -> Option<&'static str> {
    match meta.returns {
        Some(Returns::One) => Some("one"),
        Some(Returns::Maybe) => Some("maybe"),
        Some(Returns::None) => Some("none"),
        Some(Returns::Many) | None => Option::None,
    }
}

fn keys_json(keys: &[PickKeyMeta]) -> Value {
    keys.iter()
        .map(|k| json!({ "name": k.name, "required": k.required }))
        .collect()
}

/// The pgtyped IR for a query, rebuilt from meta; equals `qf_sqlfile::pgtyped_ir`.
pub fn meta_ir(meta: &QueryMeta) -> Value {
    let mut firsts: Vec<(u32, &str)> = meta
        .params
        .iter()
        .filter_map(|p| {
            p.locs
                .iter()
                .map(|l| l.a)
                .min()
                .map(|a| (a, p.name.as_str()))
        })
        .collect();
    firsts.sort_by_key(|(a, _)| *a);
    let mut used = Map::new();
    for (_, n) in firsts {
        used.insert(n.to_string(), Value::Bool(true));
    }
    let params: Vec<Value> = meta
        .params
        .iter()
        .map(|p| {
            let mut o = Map::new();
            o.insert("name".into(), p.name.clone().into());
            if !p.locs.is_empty() {
                o.insert("required".into(), p.required.into());
            }
            let transform = match &p.transform {
                Transform::Scalar => json!({ "type": "scalar" }),
                Transform::Spread => json!({ "type": "array_spread" }),
                Transform::Pick { keys } => {
                    json!({ "type": "pick_tuple", "keys": keys_json(keys) })
                }
                Transform::PickSpread { keys } => {
                    json!({ "type": "pick_array_spread", "keys": keys_json(keys) })
                }
            };
            o.insert("transform".into(), transform);
            let locs: Vec<Value> = p
                .locs
                .iter()
                .map(|l| json!({ "a": l.a, "b": l.b }))
                .collect();
            o.insert("locs".into(), locs.into());
            Value::Object(o)
        })
        .collect();
    let mut ir = Map::new();
    ir.insert("usedParamSet".into(), Value::Object(used));
    ir.insert("params".into(), params.into());
    ir.insert("statement".into(), meta.statement.source.clone().into());
    Value::Object(ir)
}

impl RuntimeBinding for PgtypedBinding {
    fn imports(&self, meta: &QueryMeta) -> Vec<String> {
        wrapper(meta)
            .map(|w| format!("import {{ {w} }} from 'queryfirst-runtime';"))
            .into_iter()
            .collect()
    }

    fn query_export(&self, meta: &QueryMeta, params_alias: &str, result_alias: &str) -> String {
        let n = &meta.name;
        let pq = format!("new PreparedQuery<{params_alias},{result_alias}>({n}IR)");
        match wrapper(meta) {
            Some(w) => format!("export const {n} = {w}({pq}, \"{n}\");\n"),
            None => format!("export const {n} = {pq};\n"),
        }
    }
}
