//! Finding query files. Config globs are matched against project-relative POSIX paths; paths
//! given on the command line are literal (files or directories) and never globbed, so Next.js
//! directories such as `app/orders/[id]/(queries)/` work as they are.

use std::path::{Path, PathBuf};

use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};
use qf_sqlfile::{Diagnostic, Severity};

use crate::config::Config;
use crate::posix;

/// Compile one config glob: `*` and `?` never match `/`, `**` spans directories.
pub fn glob(pattern: &str) -> Result<Glob, globset::Error> {
    GlobBuilder::new(pattern).literal_separator(true).build()
}

fn glob_set(patterns: &[String]) -> GlobSet {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        // Validated by `parse_config`; an invalid pattern here matches nothing.
        if let Ok(g) = glob(p) {
            b.add(g);
        }
    }
    b.build().unwrap_or_else(|_| GlobSet::empty())
}

/// Include and exclude globs of a project.
pub struct Matcher {
    include: GlobSet,
    exclude: GlobSet,
}

impl Matcher {
    pub fn new(cfg: &Config) -> Self {
        Matcher {
            include: glob_set(&cfg.include),
            exclude: glob_set(&cfg.exclude),
        }
    }

    pub fn excluded(&self, rel: &str) -> bool {
        self.exclude.is_match(rel)
    }

    pub fn is_query(&self, rel: &str) -> bool {
        self.include.is_match(rel) && !self.excluded(rel)
    }

    /// Whether everything below the directory `rel` is excluded, so the walk can skip it.
    fn prunes(&self, rel: &str) -> bool {
        // A name that cannot be a real file, one and two levels down.
        let probe = "\u{1}";
        !rel.is_empty()
            && self.excluded(&format!("{rel}/{probe}"))
            && self.excluded(&format!("{rel}/{probe}/{probe}"))
    }
}

fn usage_error(message: String) -> Diagnostic {
    Diagnostic {
        code: "QF091",
        severity: Severity::Error,
        message,
        hint: None,
        line: 1,
        col: 1,
    }
}

/// Walk `dir` (project-relative `rel`), calling `visit` with the project-relative POSIX path of
/// every file. Does not follow symbolic links; skips `.git` and directories the matcher prunes.
pub fn walk(
    project: &Path,
    rel: &str,
    m: &Matcher,
    visit: &mut dyn FnMut(&str),
) -> std::io::Result<()> {
    let dir = if rel.is_empty() {
        project.to_path_buf()
    } else {
        project.join(rel)
    };
    let mut entries: Vec<(String, bool)> = Vec::new();
    for e in std::fs::read_dir(&dir)? {
        let e = e?;
        let ft = e.file_type()?;
        let name = e.file_name().to_string_lossy().into_owned();
        if ft.is_dir() {
            entries.push((name, true));
        } else if ft.is_file() {
            entries.push((name, false));
        }
    }
    entries.sort();
    for (name, is_dir) in entries {
        let child = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        if is_dir {
            if name == ".git" || m.prunes(&child) {
                continue;
            }
            walk(project, &child, m, visit)?;
        } else {
            visit(&child);
        }
    }
    Ok(())
}

/// The canonical project directory, for computing relative paths.
fn canonical(p: &Path) -> Result<PathBuf, Diagnostic> {
    std::fs::canonicalize(p).map_err(|e| usage_error(format!("cannot access {}: {e}", p.display())))
}

/// The query files in scope, as project-relative paths, sorted and without duplicates.
/// Without `cli_paths` that is the whole project. Relative `cli_paths` are relative to `project`
/// (the CLI makes them absolute first). A file argument must be a query file by the config's
/// include/exclude globs; a directory argument is walked like the project.
pub fn discover(
    project: &Path,
    cfg: &Config,
    cli_paths: &[PathBuf],
) -> Result<Vec<PathBuf>, Diagnostic> {
    let m = Matcher::new(cfg);
    let root = canonical(project)?;
    let mut found: Vec<String> = Vec::new();
    let walk_dir = |rel: &str, found: &mut Vec<String>| {
        walk(&root, rel, &m, &mut |f| {
            if m.is_query(f) {
                found.push(f.to_string());
            }
        })
        .map_err(|e| {
            usage_error(format!(
                "cannot read directory {}: {e}",
                root.join(rel).display()
            ))
        })
    };
    if cli_paths.is_empty() {
        walk_dir("", &mut found)?;
    }
    for arg in cli_paths {
        let abs = if arg.is_absolute() {
            arg.clone()
        } else {
            project.join(arg)
        };
        if !abs.exists() {
            return Err(usage_error(format!(
                "no such file or directory: {}",
                arg.display()
            )));
        }
        let abs = canonical(&abs)?;
        let rel = abs.strip_prefix(&root).map_err(|_| {
            usage_error(format!(
                "{} is outside the project directory {}",
                arg.display(),
                project.display()
            ))
        })?;
        let rel = posix(rel);
        if abs.is_dir() {
            walk_dir(&rel, &mut found)?;
        } else if m.is_query(&rel) {
            found.push(rel);
        } else {
            return Err(usage_error(format!(
                "{rel} is not a query file: it does not match the include globs, or matches the \
                 exclude globs, of the project configuration"
            )));
        }
    }
    found.sort();
    found.dedup();
    Ok(found.into_iter().map(PathBuf::from).collect())
}
