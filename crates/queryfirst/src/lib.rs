//! Queryfirst: the `qf` command. Discovers query files, describes them against the database,
//! assembles meta files, runs the generators and owns every file write.

pub mod cmd;
pub mod config;
pub mod discover;
pub mod engine;
pub mod report;
pub mod writer;

use std::path::Path;

/// A project-relative path as POSIX text (`/` separators), as stored in meta files and reports.
pub fn posix(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}
