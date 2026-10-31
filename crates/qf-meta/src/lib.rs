//! qf-meta: the versioned `.qf.json` meta model, canonical serialisation and hashing.

mod hash;
mod io;
mod model;

pub use hash::sha256_tag;
pub use io::{MetaError, from_json, meta_path_for, to_canonical_json};
pub use model::*;
