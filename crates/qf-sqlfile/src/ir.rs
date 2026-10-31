//! pgtyped IR, ported from `queryASTToIR` (packages/parser/src/loader/sql/index.ts).

use crate::{ExpansionKind, LexedQuery, ParamUse, PickKey, QueryFile};
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, Copy)]
pub(crate) enum Transform<'a> {
    Scalar,
    Spread,
    Pick(&'a [PickKey]),
    PickSpread(&'a [PickKey]),
}

pub(crate) struct IrParam<'a> {
    pub name: &'a str,
    pub transform: Transform<'a>,
    /// Indexes into the uses.
    pub uses: Vec<usize>,
}

/// pgtyped's param order: `@param` declarations first (in order, used or not), then the other
/// params in order of first use.
pub(crate) fn ordered_params<'a>(q: &'a QueryFile, uses: &'a [ParamUse]) -> Vec<IrParam<'a>> {
    let mut params: Vec<IrParam> = q
        .expansions
        .iter()
        .map(|e| IrParam {
            name: &e.param,
            transform: match &e.kind {
                ExpansionKind::Spread => Transform::Spread,
                ExpansionKind::Pick(k) => Transform::Pick(k),
                ExpansionKind::PickSpread(k) => Transform::PickSpread(k),
            },
            uses: Vec::new(),
        })
        .collect();
    for (k, u) in uses.iter().enumerate() {
        match params.iter_mut().find(|p| p.name == u.name) {
            Some(p) => p.uses.push(k),
            None => params.push(IrParam {
                name: &u.name,
                transform: Transform::Scalar,
                uses: vec![k],
            }),
        }
    }
    params
}

fn keys_json(keys: &[PickKey]) -> Value {
    keys.iter()
        .map(|k| json!({ "name": k.name, "required": k.required }))
        .collect()
}

/// Build the IR object `@pgtyped/runtime` reads. `required` gives the final decision per used
/// param; params declared with `@param` but never used get no `required` key, as in pgtyped.
pub fn pgtyped_ir(q: &QueryFile, lexed: &LexedQuery, required: &dyn Fn(&str) -> bool) -> Value {
    let mut used = Map::new();
    for u in &lexed.uses {
        used.entry(u.name.clone()).or_insert(Value::Bool(true));
    }
    let params: Vec<Value> = ordered_params(q, &lexed.uses)
        .into_iter()
        .map(|p| {
            let mut o = Map::new();
            o.insert("name".into(), p.name.into());
            if !p.uses.is_empty() {
                o.insert("required".into(), required(p.name).into());
            }
            let transform = match p.transform {
                Transform::Scalar => json!({ "type": "scalar" }),
                Transform::Spread => json!({ "type": "array_spread" }),
                Transform::Pick(k) => json!({ "type": "pick_tuple", "keys": keys_json(k) }),
                Transform::PickSpread(k) => {
                    json!({ "type": "pick_array_spread", "keys": keys_json(k) })
                }
            };
            o.insert("transform".into(), transform);
            let locs: Vec<Value> = p
                .uses
                .iter()
                .map(|&k| {
                    let l = lexed.uses[k].loc;
                    json!({ "a": l.a, "b": l.b })
                })
                .collect();
            o.insert("locs".into(), locs.into());
            Value::Object(o)
        })
        .collect();
    let mut ir = Map::new();
    ir.insert("usedParamSet".into(), Value::Object(used));
    ir.insert("params".into(), params.into());
    ir.insert("statement".into(), lexed.statement.clone().into());
    Value::Object(ir)
}
