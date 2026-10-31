//! All file writes go through here: LF only, a trailing newline, and nothing written when the
//! bytes on disk are already right.

use std::io;
use std::path::Path;

/// `content` with CRLF and CR turned into LF, ending in a newline.
pub fn normalise_text(content: &str) -> String {
    let mut s = qf_sqlfile::normalise(content);
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

/// Write `content` (normalised, see [`normalise_text`]) to `path` unless the file already holds
/// exactly those bytes. Creates parent directories. Returns whether the file was written.
/// The write goes to a temporary sibling first and is renamed into place.
pub fn write_if_changed(path: &Path, content: &str) -> io::Result<bool> {
    let content = normalise_text(content);
    match std::fs::read(path) {
        Ok(existing) if existing == content.as_bytes() => return Ok(false),
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{file_name}.qf-tmp-{}", std::process::id()));
    std::fs::write(&tmp, content.as_bytes())?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(true)
}

/// Delete `path` if it exists. Returns whether it existed.
pub fn remove_if_exists(path: &Path) -> io::Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}
