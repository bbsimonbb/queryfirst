use crate::{META_VERSION, QueryMeta};
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum MetaError {
    /// The file was written by a newer Queryfirst.
    FutureVersion(u64),
    Json(serde_json::Error),
}

impl fmt::Display for MetaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MetaError::FutureVersion(v) => write!(
                f,
                "meta file version {v} was written by a newer Queryfirst (this one supports up to version {META_VERSION}); upgrade qf"
            ),
            MetaError::Json(e) => write!(f, "invalid meta file: {e}"),
        }
    }
}

impl std::error::Error for MetaError {}

/// Pretty JSON, 2-space indent, LF, trailing newline, keys in declaration order.
pub fn to_canonical_json(meta: &QueryMeta) -> String {
    let mut s = serde_json::to_string_pretty(meta).expect("meta serialises");
    s.push('\n');
    s
}

/// Parse a meta file, rejecting versions newer than [`META_VERSION`].
pub fn from_json(s: &str) -> Result<QueryMeta, MetaError> {
    let value: serde_json::Value = serde_json::from_str(s).map_err(MetaError::Json)?;
    if let Some(v) = value.get("version").and_then(|v| v.as_u64())
        && v > u64::from(META_VERSION)
    {
        return Err(MetaError::FutureVersion(v));
    }
    serde_json::from_value(value).map_err(MetaError::Json)
}

/// `foo.psql` -> `foo.qf.json` (same directory).
pub fn meta_path_for(source: &Path) -> PathBuf {
    source.with_extension("qf.json")
}
