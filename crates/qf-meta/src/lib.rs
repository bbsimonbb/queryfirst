//! qf-meta: the versioned `.qf.json` meta model, the deployment manifest, canonical
//! serialisation and hashing.

mod diff;
mod hash;
mod io;
mod manifest;
mod model;

pub use diff::{Difference, compare};
pub use hash::sha256_tag;
pub use io::{MetaError, from_json, meta_path_for, to_canonical_json};
pub use manifest::{
    MANIFEST_FILE, MANIFEST_VERSION, Manifest, ManifestSettings, manifest_from_json,
    manifest_to_json,
};
pub use model::*;
