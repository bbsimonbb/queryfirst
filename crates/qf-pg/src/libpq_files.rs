//! libpq's password file (`.pgpass`) and connection service file (`pg_service.conf`).
//!
//! Errors and warnings about these files may name the file, a line number or a libpq keyword,
//! never a line's content: the files hold passwords.

use std::path::{Path, PathBuf};

use crate::connect::{ConnError, Env, env_nonempty};

/// libpq's per-user directory: `%APPDATA%\postgresql` on Windows, `$HOME` elsewhere.
fn home_dir(env: Env) -> Option<PathBuf> {
    if cfg!(windows) {
        env_nonempty(env, "APPDATA").map(|a| PathBuf::from(a).join("postgresql"))
    } else {
        env_nonempty(env, "HOME").map(PathBuf::from)
    }
}

/// The default password file, used when neither `passfile` nor `PGPASSFILE` names one:
/// `%APPDATA%\postgresql\pgpass.conf` on Windows, `~/.pgpass` elsewhere.
pub(crate) fn default_pgpass(env: Env) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "pgpass.conf"
    } else {
        ".pgpass"
    };
    home_dir(env).map(|h| h.join(name))
}

/// What a password-file line is matched against.
pub(crate) struct PassKey<'a> {
    pub host: &'a str,
    pub port: &'a str,
    pub dbname: &'a str,
    pub user: &'a str,
}

/// The password of the first line of the password file `path` matching `key`, as libpq looks it
/// up: `hostname:port:database:username:password`, a field of just `*` matches anything, `\`
/// escapes the next character, lines starting with `#` are comments. A missing or unreadable
/// file is no password. On Unix, a file that is not a plain file, or that the group or others
/// can access, is ignored with a warning on stderr, as libpq does.
pub(crate) fn pgpass_lookup(path: &Path, key: &PassKey<'_>) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if !meta.is_file() {
            eprintln!(
                "warning: password file {} is not a plain file; ignoring it",
                path.display()
            );
            return None;
        }
        if meta.permissions().mode() & 0o077 != 0 {
            eprintln!(
                "warning: password file {} has group or world access; permissions should be \
                 u=rw (0600) or less; ignoring it",
                path.display()
            );
            return None;
        }
    }
    #[cfg(not(unix))]
    if !meta.is_file() {
        return None;
    }
    let text = String::from_utf8_lossy(&std::fs::read(path).ok()?).into_owned();
    pgpass_match(&text, key)
}

/// [`pgpass_lookup`] on the file's text.
pub(crate) fn pgpass_match(text: &str, key: &PassKey<'_>) -> Option<String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let matched = [key.host, key.port, key.dbname, key.user]
            .into_iter()
            .try_fold(line, |rest, want| match_field(rest, want));
        if let Some(rest) = matched {
            let pw = unescape_field(rest);
            return (!pw.is_empty()).then_some(pw);
        }
    }
    None
}

/// If the first field of `line` matches `want`, what follows its `:`.
fn match_field<'a>(line: &'a str, want: &str) -> Option<&'a str> {
    if let Some(rest) = line.strip_prefix("*:") {
        return Some(rest);
    }
    let mut value = String::new();
    let mut chars = line.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                if let Some((_, next)) = chars.next() {
                    value.push(next);
                }
            }
            ':' => return (value == want).then(|| &line[i + 1..]),
            c => value.push(c),
        }
    }
    // Fewer than five fields: the line matches nothing.
    None
}

/// The password field: up to an unescaped `:` or the end, escapes removed.
fn unescape_field(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(next) => out.push(next),
                None => out.push('\\'),
            },
            ':' => break,
            c => out.push(c),
        }
    }
    out
}

/// One `key=value` line of a service-file section.
pub(crate) struct ServiceEntry {
    pub key: String,
    pub value: String,
    pub line: usize,
}

/// A service's section, and the file it came from.
pub(crate) struct ServiceDef {
    pub file: PathBuf,
    pub entries: Vec<ServiceEntry>,
}

/// The service files, in libpq's order: `PGSERVICEFILE`, else the per-user
/// `~/.pg_service.conf` (`%APPDATA%\postgresql\.pg_service.conf` on Windows); then the system
/// file `PGSYSCONFDIR/pg_service.conf`.
fn service_files(env: Env) -> Vec<PathBuf> {
    let user = env_nonempty(env, "PGSERVICEFILE")
        .map(PathBuf::from)
        .or_else(|| home_dir(env).map(|h| h.join(".pg_service.conf")));
    let system =
        env_nonempty(env, "PGSYSCONFDIR").map(|d| PathBuf::from(d).join("pg_service.conf"));
    [user, system].into_iter().flatten().collect()
}

/// The section `[name]` of the first service file that has one. A file that does not exist is
/// skipped; none having the section is a `QF091` error naming the files searched.
pub(crate) fn find_service(env: Env, name: &str) -> Result<ServiceDef, ConnError> {
    let files = service_files(env);
    for file in &files {
        if !file.exists() {
            continue;
        }
        if let Some(entries) = parse_service_file(file, name)? {
            return Ok(ServiceDef {
                file: file.clone(),
                entries,
            });
        }
    }
    let searched = if files.is_empty() {
        "no service file to search".to_string()
    } else {
        let names: Vec<String> = files.iter().map(|f| f.display().to_string()).collect();
        format!("searched {}", names.join(", "))
    };
    Err(
        ConnError::new("QF091", format!("definition of service `{name}` not found"))
            .with_detail(searched)
            .with_hint(
                "define the service in the file named by PGSERVICEFILE, in ~/.pg_service.conf \
                 (Windows: %APPDATA%\\postgresql\\.pg_service.conf) or in \
                 PGSYSCONFDIR/pg_service.conf",
            ),
    )
}

/// The entries of section `[name]` of the service file `path`, `None` if it has no such section.
fn parse_service_file(path: &Path, name: &str) -> Result<Option<Vec<ServiceEntry>>, ConnError> {
    let bytes = std::fs::read(path).map_err(|e| {
        ConnError::new(
            "QF091",
            format!("cannot read service file {}", path.display()),
        )
        .with_detail(e.to_string())
    })?;
    parse_service_text(&String::from_utf8_lossy(&bytes), name)
        .map_err(|line| syntax_error(path, line))
}

/// The section `[name]` of a service file's text; `Err(line)` for a line in it without `=`.
pub(crate) fn parse_service_text(
    text: &str,
    name: &str,
) -> Result<Option<Vec<ServiceEntry>>, usize> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut found = false;
    let mut in_group = false;
    let mut entries = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            if found {
                // The end of the section.
                break;
            }
            in_group = rest
                .strip_prefix(name)
                .is_some_and(|after| after.starts_with(']'));
            found = in_group;
            continue;
        }
        if !in_group {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(i + 1);
        };
        entries.push(ServiceEntry {
            key: key.trim().to_string(),
            value: value.trim().to_string(),
            line: i + 1,
        });
    }
    Ok(found.then_some(entries))
}

pub(crate) fn syntax_error(path: &Path, line: usize) -> ConnError {
    ConnError::new(
        "QF091",
        format!(
            "syntax error in service file {}, line {line}",
            path.display()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key<'a>(host: &'a str, port: &'a str, dbname: &'a str, user: &'a str) -> PassKey<'a> {
        PassKey {
            host,
            port,
            dbname,
            user,
        }
    }

    #[test]
    fn pgpass_first_match_wins() {
        let text = "h:5432:db:u:first\nh:5432:db:u:second\n";
        assert_eq!(
            pgpass_match(text, &key("h", "5432", "db", "u")).as_deref(),
            Some("first")
        );
    }

    #[test]
    fn pgpass_wildcards_and_fields() {
        let text = "\
# a comment line: h:5432:db:u:commented
other:5432:db:u:wronghost
h:6000:db:u:wrongport
*:5432:*:u:wild
*:*:*:*:any
";
        assert_eq!(
            pgpass_match(text, &key("h", "5432", "db", "u")).as_deref(),
            Some("wild")
        );
        assert_eq!(
            pgpass_match(text, &key("h", "6000", "db", "u")).as_deref(),
            Some("wrongport")
        );
        assert_eq!(
            pgpass_match(text, &key("x", "1", "y", "z")).as_deref(),
            Some("any")
        );
        assert_eq!(
            pgpass_match("h:5432:db:u:p", &key("h", "5432", "db", "v")),
            None
        );
        // A partial field of `*` is literal, not a wildcard.
        assert_eq!(
            pgpass_match("h*:5432:db:u:p", &key("h1", "5432", "db", "u")),
            None
        );
    }

    #[test]
    fn pgpass_escapes() {
        // `\:` and `\\` in the match fields and in the password.
        let text = r"my\:host:5432:d\\b:u:pa\:ss\\word";
        assert_eq!(
            pgpass_match(text, &key("my:host", "5432", r"d\b", "u")).as_deref(),
            Some(r"pa:ss\word")
        );
        // An escaped `*` is a literal star.
        assert_eq!(
            pgpass_match(r"\*:5432:db:u:p", &key("h", "5432", "db", "u")),
            None
        );
        assert_eq!(
            pgpass_match(r"\*:5432:db:u:p", &key("*", "5432", "db", "u")).as_deref(),
            Some("p")
        );
        // The password ends at an unescaped colon.
        assert_eq!(
            pgpass_match("h:5432:db:u:pw:extra", &key("h", "5432", "db", "u")).as_deref(),
            Some("pw")
        );
    }

    #[test]
    fn pgpass_crlf_short_lines_and_bom() {
        let text = "\u{feff}h:5432:db\r\nh:5432:db:u:crlf\r\n";
        assert_eq!(
            pgpass_match(text, &key("h", "5432", "db", "u")).as_deref(),
            Some("crlf")
        );
    }

    #[test]
    fn pgpass_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            pgpass_lookup(&dir.path().join("nope"), &key("h", "5432", "db", "u")),
            None
        );
        // A directory is no password file either.
        assert_eq!(
            pgpass_lookup(dir.path(), &key("h", "5432", "db", "u")),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn pgpass_group_readable_is_ignored() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("pgpass");
        std::fs::write(&f, "*:*:*:*:pw\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert_eq!(pgpass_lookup(&f, &key("h", "5432", "db", "u")), None);
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            pgpass_lookup(&f, &key("h", "5432", "db", "u")).as_deref(),
            Some("pw")
        );
    }

    #[test]
    fn service_sections() {
        let text = "\
# comment
[other]
host=wrong

[mydb]
  host = db.example
port=6000
bad line is not here
[mydb2]
host=never
";
        // The syntax error is inside [mydb].
        assert_eq!(parse_service_text(text, "mydb").err(), Some(8));
        let text = text.replace("bad line is not here\n", "");
        let e = parse_service_text(&text, "mydb").unwrap().unwrap();
        let kv: Vec<_> = e.iter().map(|e| (&*e.key, &*e.value)).collect();
        assert_eq!(kv, [("host", "db.example"), ("port", "6000")]);
        // Prefix of a section name is not the section; a later line outside it is not checked.
        assert!(parse_service_text(&text, "my").unwrap().is_none());
        assert!(
            parse_service_text("[a]\nx=1\n[b]\nnot ok", "a")
                .unwrap()
                .is_some()
        );
        assert!(parse_service_text(&text, "absent").unwrap().is_none());
    }
}
