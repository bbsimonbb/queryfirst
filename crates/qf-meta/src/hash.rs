use sha2::{Digest, Sha256};
use std::fmt::Write;

/// `"sha256:<lowercase hex>"` of `bytes`.
pub fn sha256_tag(bytes: &[u8]) -> String {
    let mut s = String::from("sha256:");
    for b in Sha256::digest(bytes) {
        write!(s, "{b:02x}").expect("write to String");
    }
    s
}
