//! `queryfirst.manifest.json`: every meta file of a project in one document, written by
//! `qf bundle` and read by `qf test --manifest` in a deployment, where no source tree exists.

use serde::{Deserialize, Serialize};

use crate::io::MetaError;
use crate::{META_VERSION, QueryMeta};

pub const MANIFEST_VERSION: u32 = 1;
/// The default manifest file name, at the project root.
pub const MANIFEST_FILE: &str = "queryfirst.manifest.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub version: u32,
    pub settings: ManifestSettings,
    /// Sorted by `source`.
    pub queries: Vec<QueryMeta>,
}

/// The project settings `qf test` needs without a `queryfirst.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestSettings {
    /// `queryfirst.json`'s `timestamp` (`"utc"` enables the time zone check).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

impl Manifest {
    /// A manifest of `queries`, sorted by source (then name, for determinism).
    pub fn new(settings: ManifestSettings, mut queries: Vec<QueryMeta>) -> Self {
        queries.sort_by(|a, b| (&a.source, &a.name).cmp(&(&b.source, &b.name)));
        Manifest {
            version: MANIFEST_VERSION,
            settings,
            queries,
        }
    }
}

/// Pretty JSON, 2-space indent, LF, trailing newline, keys in declaration order.
pub fn manifest_to_json(m: &Manifest) -> String {
    let mut s = serde_json::to_string_pretty(m).expect("manifest serialises");
    s.push('\n');
    s
}

fn future(v: &serde_json::Value, max: u32, err: fn(u64) -> MetaError) -> Result<(), MetaError> {
    match v.get("version").and_then(|v| v.as_u64()) {
        Some(n) if n > u64::from(max) => Err(err(n)),
        _ => Ok(()),
    }
}

/// Parse a manifest, rejecting a manifest or a query meta newer than this Queryfirst.
pub fn manifest_from_json(s: &str) -> Result<Manifest, MetaError> {
    let value: serde_json::Value = serde_json::from_str(s).map_err(MetaError::Json)?;
    future(&value, MANIFEST_VERSION, MetaError::FutureManifestVersion)?;
    if let Some(queries) = value.get("queries").and_then(|q| q.as_array()) {
        for q in queries {
            future(q, META_VERSION, MetaError::FutureVersion)?;
        }
    }
    serde_json::from_value(value).map_err(MetaError::Json)
}
