//! Connection resolution (explicit, env var, standard env, helper command), TLS and the pool.
//!
//! Connection strings are never logged or written: [`ConnSource`], [`ResolvedConn`] and
//! [`ConnError`] never show a password or a resolved connection string.

use std::fmt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_postgres::Client;
use tokio_postgres::config::Host;
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::libpq_files::{PassKey, default_pgpass, find_service, pgpass_lookup, syntax_error};

/// Where a connection string may come from. [`resolve_connection`] takes the first that yields
/// a value.
#[derive(Clone, PartialEq, Eq)]
pub enum ConnSource {
    /// A connection string given directly (URL or `key=value` form).
    Explicit(String),
    /// The name of an environment variable holding a connection string.
    Env(String),
    /// `DATABASE_URL`, else the libpq `PG*` variables.
    StandardEnv,
    /// A helper command run through the platform shell; its trimmed stdout is the string.
    Command(String),
}

impl fmt::Debug for ConnSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnSource::Explicit(_) => f.write_str("Explicit(<redacted>)"),
            ConnSource::Env(name) => f.debug_tuple("Env").field(name).finish(),
            ConnSource::StandardEnv => f.write_str("StandardEnv"),
            ConnSource::Command(cmd) => f.debug_tuple("Command").field(&redact(cmd)).finish(),
        }
    }
}

/// A connection-level failure: `QF090` (cannot reach the database) or `QF091` (config error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnError {
    pub code: &'static str,
    pub message: String,
    /// The underlying cause, never containing a connection string.
    pub detail: Option<String>,
    /// What to do about it, when there is something specific to say.
    pub hint: Option<String>,
}

impl ConnError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        ConnError {
            code,
            message: message.into(),
            detail: None,
            hint: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        let d = detail.into();
        self.detail = (!d.trim().is_empty()).then(|| d.trim().to_string());
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub(crate) fn unreachable(target: &str, cause: impl Into<String>) -> Self {
        ConnError::new("QF090", format!("cannot reach database at {target}")).with_detail(cause)
    }

    fn config(message: impl Into<String>) -> Self {
        ConnError::new("QF091", message)
    }
}

impl fmt::Display for ConnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if let Some(d) = &self.detail {
            write!(f, ": {d}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConnError {}

/// libpq's `sslmode`. `verify-ca` and `verify-full` both verify the server certificate and host
/// name, against the `sslrootcert` file when one is given, else the Mozilla roots. `require`
/// verifies the same way when an `sslrootcert` is given (as libpq does when it has a root
/// certificate), else encrypts without verifying. `no-verify` (node-postgres' convention, not
/// libpq's) always encrypts without verifying, `sslrootcert` or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SslMode {
    Disable,
    Prefer,
    Require,
    NoVerify,
    VerifyCa,
    VerifyFull,
}

impl SslMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SslMode::Disable => "disable",
            SslMode::Prefer => "prefer",
            SslMode::Require => "require",
            SslMode::NoVerify => "no-verify",
            SslMode::VerifyCa => "verify-ca",
            SslMode::VerifyFull => "verify-full",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "disable" => SslMode::Disable,
            // libpq's `allow` tries plaintext first; TLS-if-offered is the closest we have.
            "allow" | "prefer" => SslMode::Prefer,
            "require" => SslMode::Require,
            "no-verify" => SslMode::NoVerify,
            "verify-ca" => SslMode::VerifyCa,
            "verify-full" => SslMode::VerifyFull,
            _ => return None,
        })
    }

    /// Whether the server certificate is verified, given whether an `sslrootcert` was set.
    fn verifies(self, root_cert: bool) -> bool {
        match self {
            SslMode::VerifyCa | SslMode::VerifyFull => true,
            SslMode::Require => root_cert,
            SslMode::Disable | SslMode::Prefer | SslMode::NoVerify => false,
        }
    }
}

/// libpq's `sslrootcert` value meaning "the system's trusted roots" (here, the Mozilla roots).
const SYSTEM_ROOTS: &str = "system";

/// A resolved connection configuration. `Display` and `Debug` never show the password.
#[derive(Clone)]
pub struct ResolvedConn {
    config: tokio_postgres::Config,
    ssl: SslMode,
    /// `sslrootcert` (or `PGSSLROOTCERT`): a PEM file of trusted root certificates.
    root_cert: Option<String>,
    host_port: String,
    /// `host:port/db`.
    pub display_target: String,
}

impl ResolvedConn {
    pub fn ssl_mode(&self) -> SslMode {
        self.ssl
    }

    /// The `sslrootcert` path (from the connection string, else `PGSSLROOTCERT`), if any.
    pub fn root_cert(&self) -> Option<&str> {
        self.root_cert.as_deref()
    }

    /// `host:port`, as used in `QF090` messages.
    pub fn host_port(&self) -> &str {
        &self.host_port
    }

    /// The same connection, with every session opened read-only
    /// (`-c default_transaction_read_only=on` appended to the startup `options`, after any the
    /// connection string gave, so it wins). Defence in depth for `qf test` against production:
    /// describes and catalog reads never write anyway.
    pub fn read_only(mut self) -> Self {
        let opts = match self.config.get_options() {
            Some(o) if !o.trim().is_empty() => format!("{} {READ_ONLY_OPTION}", o.trim()),
            _ => READ_ONLY_OPTION.to_string(),
        };
        self.config.options(&opts);
        self
    }

    /// The startup `options` sent to the server (`-c name=value …`), if any.
    pub fn options(&self) -> Option<&str> {
        self.config.get_options()
    }
}

/// See [`ResolvedConn::read_only`].
pub const READ_ONLY_OPTION: &str = "-c default_transaction_read_only=on";

impl fmt::Display for ResolvedConn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display_target)
    }
}

impl fmt::Debug for ResolvedConn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedConn")
            .field("target", &self.display_target)
            .field("user", &self.config.get_user())
            .field(
                "password",
                &self.config.get_password().map(|_| "<redacted>"),
            )
            .field("sslmode", &self.ssl.as_str())
            .field("sslrootcert", &self.root_cert)
            .finish()
    }
}

/// An environment lookup: the process environment, or one a test injects.
pub(crate) type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

/// `name` from `env`, unless unset or blank.
pub(crate) fn env_nonempty(env: Env, name: &str) -> Option<String> {
    env(name).filter(|v| !v.trim().is_empty())
}

fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Any of these makes the `PG*` variables a connection source of their own.
const PG_SOURCE_VARS: [&str; 6] = [
    "PGHOST",
    "PGPORT",
    "PGUSER",
    "PGDATABASE",
    "PGPASSWORD",
    "PGSERVICE",
];

/// Resolve the first source that yields a connection string, against the process environment.
/// A helper command that fails, or a string that does not parse, is a `QF091` error rather than
/// a fall-through. See [`resolve_connection_with`].
pub fn resolve_connection(sources: &[ConnSource]) -> Result<ResolvedConn, ConnError> {
    resolve_connection_with(sources, &process_env)
}

/// [`resolve_connection`] with the environment read through `env` (tests inject one).
///
/// Whatever the source, the string is completed as libpq completes it: a `service` (the
/// string's, else `PGSERVICE`) fills what the string leaves out from `pg_service.conf`; then the
/// `PG*` variables fill what is still missing; then, with no password yet, the password file
/// (`passfile`, else `PGPASSFILE`, else `~/.pgpass` / `%APPDATA%\postgresql\pgpass.conf`).
pub fn resolve_connection_with(
    sources: &[ConnSource],
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<ResolvedConn, ConnError> {
    for source in sources {
        match source {
            ConnSource::Explicit(s) if !s.trim().is_empty() => {
                return parse_conn(s.trim(), "the explicit connection string", env);
            }
            ConnSource::Explicit(_) => {}
            ConnSource::Env(name) => {
                if let Some(v) = env_nonempty(env, name) {
                    return parse_conn(v.trim(), &format!("environment variable {name}"), env);
                }
            }
            ConnSource::StandardEnv => {
                if let Some(v) = env_nonempty(env, "DATABASE_URL") {
                    return parse_conn(v.trim(), "environment variable DATABASE_URL", env);
                }
                if PG_SOURCE_VARS
                    .iter()
                    .any(|v| env_nonempty(env, v).is_some())
                {
                    return parse_conn("", "the PG* environment variables", env);
                }
            }
            ConnSource::Command(cmd) => {
                let out = run_helper(cmd)?;
                return parse_conn(&out, &format!("the output of `{}`", redact(cmd)), env);
            }
        }
    }
    Err(
        ConnError::config("no database connection configured").with_detail(
            "pass a connection string, set DATABASE_URL, PGSERVICE or the PG* variables, \
             or configure a connection helper command",
        ),
    )
}

fn run_helper(cmd: &str) -> Result<String, ConnError> {
    // The command text and its stderr are shown in errors; either may hold a secret.
    let shown = redact(cmd);
    let mut command = shell(cmd);
    command.stdin(Stdio::null());
    let out = command.output().map_err(|e| {
        ConnError::config(format!("cannot run connection helper `{shown}`"))
            .with_detail(redact(&e.to_string()))
    })?;
    let stderr = redact(&String::from_utf8_lossy(&out.stderr));
    if !out.status.success() {
        return Err(ConnError::config(format!(
            "connection helper `{shown}` failed ({})",
            out.status
        ))
        .with_detail(stderr));
    }
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if stdout.is_empty() {
        return Err(
            ConnError::config(format!("connection helper `{shown}` printed nothing"))
                .with_detail(stderr),
        );
    }
    Ok(stdout)
}

/// `text` with anything that looks like a connection secret masked: `postgres://` and
/// `postgresql://` URLs (any case, up to whitespace or a quote) become `postgres://***`, and the
/// value of any `password=` (any case, so `PGPASSWORD=` too; spaces around `=` allowed; value
/// bare, single- or double-quoted) becomes `***`. For text Queryfirst does not control, such as
/// a helper command and its stderr, before it is shown.
pub fn redact(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let b = text.as_bytes();
    let ends_token =
        |c: u8| c.is_ascii_whitespace() || matches!(c, b'"' | b'\'' | b'`' | b'<' | b'>');
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < b.len() {
        let rest = &lower[i..];
        if let Some(scheme) = ["postgresql://", "postgres://"]
            .into_iter()
            .find(|s| rest.starts_with(s))
        {
            let mut j = i + scheme.len();
            while j < b.len() && !ends_token(b[j]) {
                j += 1;
            }
            out.push_str("postgres://***");
            i = j;
            continue;
        }
        if rest.starts_with("password") {
            let mut j = i + "password".len();
            while j < b.len() && b[j] == b' ' {
                j += 1;
            }
            if b.get(j) == Some(&b'=') {
                j += 1;
                while j < b.len() && b[j] == b' ' {
                    j += 1;
                }
                if let Some(&quote) = b.get(j).filter(|&&c| c == b'\'' || c == b'"') {
                    j += 1;
                    while j < b.len() && b[j] != quote {
                        j += if b[j] == b'\\' { 2 } else { 1 };
                    }
                    j = (j + 1).min(b.len());
                } else {
                    while j < b.len() && !b[j].is_ascii_whitespace() {
                        j += 1;
                    }
                }
                out.push_str(&text[i..i + "password".len()]);
                out.push_str("=***");
                i = j;
                continue;
            }
        }
        let c = text[i..].chars().next().expect("in bounds");
        out.push(c);
        i += c.len_utf8();
    }
    out
}

#[cfg(windows)]
fn shell(cmd: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut c = Command::new("cmd");
    c.arg("/C").raw_arg(cmd);
    c
}

#[cfg(not(windows))]
fn shell(cmd: &str) -> Command {
    let mut c = Command::new("sh");
    c.arg("-c").arg(cmd);
    c
}

fn parse_sslmode(value: &str) -> Result<SslMode, ConnError> {
    SslMode::parse(value).ok_or_else(|| {
        ConnError::config(format!("invalid sslmode `{value}`")).with_detail(
            "expected disable, allow, prefer, require, no-verify, verify-ca or verify-full",
        )
    })
}

/// A connection string's options as libpq keyword/value pairs, in order. A later value for a
/// keyword replaces an earlier one, as in libpq. Never `Debug`: it holds the password.
#[derive(Default)]
struct Params(Vec<(String, String)>);

impl Params {
    fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn has(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    fn set(&mut self, key: &str, value: String) {
        match self.0.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value,
            None => self.0.push((key.to_string(), value)),
        }
    }

    fn take(&mut self, key: &str) -> Option<String> {
        let i = self.0.iter().position(|(k, _)| k == key)?;
        Some(self.0.remove(i).1)
    }

    /// The libpq `key='value' …` form, for tokio-postgres.
    fn to_kv(&self) -> String {
        let quote = |v: &str| v.replace('\\', "\\\\").replace('\'', "\\'");
        let pairs: Vec<String> = self
            .0
            .iter()
            .map(|(k, v)| format!("{k}='{}'", quote(v)))
            .collect();
        pairs.join(" ")
    }
}

/// `s` with `%XX` escapes decoded (invalid escapes are kept as they are).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (
                b.get(i + 1).copied().and_then(hex),
                b.get(i + 2).copied().and_then(hex),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A URL or `key=value` connection string as [`Params`]. A string that does not parse is
/// reported with tokio-postgres' reason, which names an option but never a value.
fn parse_params(s: &str, origin: &str) -> Result<Params, ConnError> {
    let parsed = match s
        .strip_prefix("postgresql://")
        .or_else(|| s.strip_prefix("postgres://"))
    {
        Some(rest) => parse_url(rest),
        None => parse_kv(s),
    };
    parsed.ok_or_else(|| match tokio_postgres::Config::from_str(s) {
        Err(e) => invalid_string(origin, &e),
        Ok(_) => ConnError::config(format!("invalid connection string from {origin}")),
    })
}

/// libpq's URL form, after the scheme: `[user[:password]@][host][:port][,…][/dbname][?k=v&…]`.
/// Only what the URL gives is set: a host without a port leaves the port to the service file,
/// `PGPORT` or the default, as in libpq.
fn parse_url(rest: &str) -> Option<Params> {
    let mut p = Params::default();
    let mut rest = rest;
    // As libpq: the user info runs up to an `@` met before any `/`.
    if let Some(i) = rest
        .find(['@', '/'])
        .filter(|&i| rest.as_bytes()[i] == b'@')
    {
        let userinfo = &rest[..i];
        rest = &rest[i + 1..];
        let (user, password) = match userinfo.split_once(':') {
            Some((u, pw)) => (u, Some(pw)),
            None => (userinfo, None),
        };
        if !user.is_empty() {
            p.set("user", percent_decode(user));
        }
        if let Some(pw) = password.filter(|pw| !pw.is_empty()) {
            p.set("password", percent_decode(pw));
        }
    }
    let (hostspec, after) = rest.split_at(rest.find(['/', '?']).unwrap_or(rest.len()));
    rest = after;
    if !hostspec.is_empty() {
        let (mut hosts, mut ports) = (Vec::new(), Vec::new());
        for chunk in hostspec.split(',') {
            let (host, port) = match chunk.strip_prefix('[') {
                Some(v6) => {
                    let (host, after) = v6.split_once(']')?;
                    match after {
                        "" => (host, ""),
                        _ => (host, after.strip_prefix(':')?),
                    }
                }
                None => chunk.split_once(':').unwrap_or((chunk, "")),
            };
            hosts.push(percent_decode(host));
            ports.push(percent_decode(port));
        }
        if hosts.iter().any(|h| !h.is_empty()) {
            p.set("host", hosts.join(","));
        }
        if ports.iter().any(|p| !p.is_empty()) {
            p.set("port", ports.join(","));
        }
    }
    if let Some(path) = rest.strip_prefix('/') {
        let end = path.find('?').unwrap_or(path.len());
        if end > 0 {
            p.set("dbname", percent_decode(&path[..end]));
        }
        rest = &path[end..];
    }
    if let Some(query) = rest.strip_prefix('?') {
        for pair in query.split('&').filter(|pair| !pair.is_empty()) {
            let (k, v) = pair.split_once('=')?;
            let (k, v) = (percent_decode(k), percent_decode(v));
            // libpq's URL-only spelling of `sslmode=require`.
            if k == "ssl" && v == "true" {
                p.set("sslmode", "require".into());
            } else {
                p.set(&k, v);
            }
        }
    }
    Some(p)
}

/// libpq `key=value` form: whitespace-separated, spaces allowed around `=`, values optionally
/// single-quoted, backslash escapes in both. `None` if malformed.
fn parse_kv(s: &str) -> Option<Params> {
    let mut p = Params::default();
    let mut chars = s.chars().peekable();
    let skip_ws = |chars: &mut std::iter::Peekable<std::str::Chars<'_>>| {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
    };
    loop {
        skip_ws(&mut chars);
        if chars.peek().is_none() {
            return Some(p);
        }
        let mut key = String::new();
        while let Some(c) = chars.next_if(|&c| c != '=' && !c.is_whitespace()) {
            key.push(c);
        }
        skip_ws(&mut chars);
        if key.is_empty() || chars.next() != Some('=') {
            return None;
        }
        skip_ws(&mut chars);
        let mut value = String::new();
        if chars.next_if_eq(&'\'').is_some() {
            loop {
                match chars.next()? {
                    '\'' => break,
                    '\\' => value.push(chars.next()?),
                    c => value.push(c),
                }
            }
        } else {
            while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
                if c != '\\' {
                    value.push(c);
                } else if let Some(next) = chars.next() {
                    value.push(next);
                }
            }
        }
        p.set(&key, value);
    }
}

/// The libpq options Queryfirst accepts (tokio-postgres' list, plus its own).
const SUPPORTED_KEYS: [&str; 22] = [
    "host",
    "hostaddr",
    "port",
    "user",
    "password",
    "dbname",
    "options",
    "application_name",
    "connect_timeout",
    "tcp_user_timeout",
    "keepalives",
    "keepalives_idle",
    "keepalives_interval",
    "keepalives_retries",
    "target_session_attrs",
    "channel_binding",
    "load_balance_hosts",
    "sslnegotiation",
    "sslmode",
    "sslrootcert",
    "passfile",
    "service",
];

/// libpq options Queryfirst does not support. In a service file these are named in the error;
/// any other unknown keyword is reported by line only, since it may be a mistyped secret.
const UNSUPPORTED_LIBPQ_KEYS: [&str; 30] = [
    "sslcert",
    "sslkey",
    "sslpassword",
    "sslcertmode",
    "sslcrl",
    "sslcrldir",
    "sslsni",
    "sslcompression",
    "ssl_min_protocol_version",
    "ssl_max_protocol_version",
    "requiressl",
    "requirepeer",
    "require_auth",
    "gssencmode",
    "gsslib",
    "gssdelegation",
    "krbsrvname",
    "client_encoding",
    "fallback_application_name",
    "replication",
    "min_protocol_version",
    "max_protocol_version",
    "scram_client_key",
    "scram_server_key",
    "oauth_issuer",
    "oauth_client_id",
    "oauth_client_secret",
    "oauth_scope",
    "authtype",
    "tty",
];

/// The same list as [`SUPPORTED_KEYS`], for messages.
const SUPPORTED_OPTIONS: &str = "host, hostaddr, port, user, password, dbname, options, \
     application_name, connect_timeout, tcp_user_timeout, keepalives, keepalives_idle, \
     keepalives_interval, keepalives_retries, target_session_attrs, channel_binding, \
     load_balance_hosts, sslnegotiation, sslmode, sslrootcert, passfile and service";

/// The error for a string tokio-postgres rejected. Its parse errors name the offending option,
/// never its value.
fn invalid_string(origin: &str, e: &tokio_postgres::Error) -> ConnError {
    let cause = std::error::Error::source(e).map(|c| c.to_string());
    if let Some(key) = cause
        .as_deref()
        .and_then(|c| c.strip_prefix("unknown option `"))
        .and_then(|c| c.strip_suffix('`'))
    {
        return ConnError::config(format!("unsupported connection option `{key}` in {origin}"))
            .with_hint(format!(
                "remove `{key}` from the connection string; Queryfirst supports {SUPPORTED_OPTIONS}"
            ));
    }
    ConnError::config(format!("invalid connection string from {origin}"))
        .with_detail(cause.unwrap_or_else(|| e.to_string()))
}

/// Fill what `p` leaves out from the service `name`'s section of `pg_service.conf`.
fn apply_service(p: &mut Params, name: &str, env: Env) -> Result<(), ConnError> {
    let svc = find_service(env, name)?;
    let at = |line: usize| format!("service `{name}` ({}, line {line})", svc.file.display());
    for e in &svc.entries {
        if e.key == "service" {
            return Err(ConnError::config(format!(
                "nested service specifications are not supported in {}",
                at(e.line)
            )));
        }
        if !SUPPORTED_KEYS.contains(&e.key.as_str()) {
            if UNSUPPORTED_LIBPQ_KEYS.contains(&e.key.as_str()) {
                return Err(ConnError::config(format!(
                    "unsupported connection option `{}` in {}",
                    e.key,
                    at(e.line)
                ))
                .with_hint(format!(
                    "remove `{}` from the service; Queryfirst supports {SUPPORTED_OPTIONS}",
                    e.key
                )));
            }
            return Err(syntax_error(&svc.file, e.line)
                .with_detail(format!("not a connection option, in service `{name}`")));
        }
        // The first value in the section wins, as in libpq.
        if !p.has(&e.key) {
            p.set(&e.key, e.value.clone());
        }
    }
    Ok(())
}

/// Fill what `p` still leaves out from the libpq environment variables, then the defaults.
fn apply_env(p: &mut Params, env: Env) -> Result<(), ConnError> {
    let var = |name: &str| env_nonempty(env, name);
    if !p.has("host") {
        let hosts = match var("PGHOST") {
            Some(h) => h.split(',').map(str::trim).collect::<Vec<_>>().join(","),
            None => "localhost".into(),
        };
        p.set("host", hosts);
    }
    if !p.has("port") {
        let port = match var("PGPORT") {
            Some(v) => {
                let parts: Vec<&str> = v.split(',').map(str::trim).collect();
                if parts
                    .iter()
                    .any(|x| !x.is_empty() && x.parse::<u16>().is_err())
                {
                    return Err(ConnError::config("invalid PGPORT"));
                }
                parts.join(",")
            }
            None => "5432".into(),
        };
        p.set("port", port);
    }
    if !p.has("user") {
        let user = var("PGUSER")
            .or_else(|| var("USER"))
            .or_else(|| var("USERNAME"))
            .unwrap_or_else(|| "postgres".into());
        p.set("user", user);
    }
    if !p.has("dbname") {
        let db = var("PGDATABASE").unwrap_or_else(|| p.get("user").unwrap_or_default().into());
        p.set("dbname", db);
    }
    if !p.has("password")
        && let Some(pw) = var("PGPASSWORD")
    {
        p.set("password", pw);
    }
    if !p.has("sslmode")
        && let Some(v) = var("PGSSLMODE")
    {
        let v = v.trim();
        if SslMode::parse(v).is_none() {
            return Err(ConnError::config(format!("invalid PGSSLMODE `{v}`")));
        }
        p.set("sslmode", v.into());
    }
    if !p.has("connect_timeout")
        && let Some(v) = var("PGCONNECT_TIMEOUT")
    {
        let v = v.trim();
        if v.parse::<i64>().is_err() {
            return Err(ConnError::config("invalid PGCONNECT_TIMEOUT"));
        }
        p.set("connect_timeout", v.into());
    }
    for (key, name) in [
        ("sslrootcert", "PGSSLROOTCERT"),
        ("application_name", "PGAPPNAME"),
        ("passfile", "PGPASSFILE"),
    ] {
        if !p.has(key)
            && let Some(v) = var(name)
        {
            p.set(key, v);
        }
    }
    Ok(())
}

fn parse_conn(s: &str, origin: &str, env: Env) -> Result<ResolvedConn, ConnError> {
    let mut p = parse_params(s, origin)?;
    let service = p.take("service").or_else(|| env_nonempty(env, "PGSERVICE"));
    if let Some(name) = service.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        apply_service(&mut p, name, env)?;
    }
    apply_env(&mut p, env)?;

    // Ours: tokio-postgres knows only disable, prefer and require, and has no sslrootcert or
    // passfile.
    let ssl = match p.take("sslmode") {
        Some(v) => parse_sslmode(&v)?,
        None => SslMode::Prefer,
    };
    let root_cert = p.take("sslrootcert");
    let passfile = p.take("passfile").filter(|f| !f.is_empty());
    let mut config =
        tokio_postgres::Config::from_str(&p.to_kv()).map_err(|e| invalid_string(origin, &e))?;
    config.ssl_mode(match ssl {
        SslMode::Disable => tokio_postgres::config::SslMode::Disable,
        SslMode::Prefer => tokio_postgres::config::SslMode::Prefer,
        _ => tokio_postgres::config::SslMode::Require,
    });

    let host = match config.get_hosts().first() {
        Some(Host::Tcp(h)) => h.clone(),
        #[cfg(unix)]
        Some(Host::Unix(p)) => p.display().to_string(),
        None => "localhost".into(),
    };
    let port = config.get_ports().first().copied().unwrap_or(5432);

    // No password from the string, the service or PGPASSWORD: the password file.
    if config.get_password().is_none_or(<[u8]>::is_empty)
        && let Some(file) = passfile.map(PathBuf::from).or_else(|| default_pgpass(env))
    {
        let key = PassKey {
            host: if host.is_empty() { "localhost" } else { &host },
            port: &port.to_string(),
            dbname: config.get_dbname().unwrap_or_default(),
            user: config.get_user().unwrap_or_default(),
        };
        if let Some(pw) = pgpass_lookup(&file, &key) {
            config.password(pw);
        }
    }

    let host_port = format!("{host}:{port}");
    let display_target = format!("{host_port}/{}", config.get_dbname().unwrap_or_default());
    Ok(ResolvedConn {
        config,
        ssl,
        root_cert,
        host_port,
        display_target,
    })
}

/// Accepts any server certificate but still checks handshake signatures: libpq's
/// `sslmode=require` semantics (encryption without authentication).
#[derive(Debug)]
struct NoCertVerification(Arc<CryptoProvider>);

impl ServerCertVerifier for NoCertVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// The trusted roots: the certificates of the PEM file `path`, or the Mozilla roots for none or
/// `system`.
fn root_store(path: Option<&str>) -> Result<RootCertStore, ConnError> {
    let mut roots = RootCertStore::empty();
    let path = match path {
        None | Some(SYSTEM_ROOTS) => {
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            return Ok(roots);
        }
        Some(p) => p,
    };
    let fail = |why: String| {
        ConnError::config(format!("cannot use sslrootcert {path}"))
            .with_detail(why)
            .with_hint("sslrootcert (or PGSSLROOTCERT) must name a PEM file of CA certificates")
    };
    let pem = std::fs::read(path).map_err(|e| fail(e.to_string()))?;
    for cert in CertificateDer::pem_slice_iter(&pem) {
        let cert = cert.map_err(|e| fail(e.to_string()))?;
        roots.add(cert).map_err(|e| fail(e.to_string()))?;
    }
    if roots.is_empty() {
        return Err(fail("no certificate found in the file".into()));
    }
    Ok(roots)
}

fn tls_connector(mode: SslMode, root_cert: Option<&str>) -> Result<MakeRustlsConnect, ConnError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| ConnError::config("cannot set up TLS").with_detail(e.to_string()))?;
    let config = if mode.verifies(root_cert.is_some()) {
        let roots = root_store(root_cert)?;
        builder.with_root_certificates(roots).with_no_client_auth()
    } else {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoCertVerification(provider)))
            .with_no_client_auth()
    };
    Ok(MakeRustlsConnect::new(config))
}

struct PoolInner {
    clients: Mutex<Vec<Client>>,
    target: String,
}

/// `size` connected clients behind a semaphore. `Clone` is cheap and shares the clients.
#[derive(Clone)]
pub struct Pool {
    inner: Arc<PoolInner>,
    permits: Arc<Semaphore>,
    query_timeout: Duration,
}

/// How long one describe or catalog query may take before the connection is considered gone.
/// A tunnel that goes silent (no RST) would otherwise hang until TCP gives up, which can take
/// hours. Override with [`Pool::with_query_timeout`].
pub const DEFAULT_QUERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Idle time before TCP keepalive probes start, so dead peers are noticed on idle connections.
pub const KEEPALIVES_IDLE: Duration = Duration::from_secs(30);

impl fmt::Debug for Pool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pool")
            .field("target", &self.inner.target)
            .finish_non_exhaustive()
    }
}

/// A client checked out of the [`Pool`]; it goes back when dropped.
pub struct PooledClient {
    client: Option<Client>,
    inner: Arc<PoolInner>,
    _permit: OwnedSemaphorePermit,
}

impl std::ops::Deref for PooledClient {
    type Target = Client;
    fn deref(&self) -> &Client {
        self.client.as_ref().expect("client present until drop")
    }
}

impl Drop for PooledClient {
    fn drop(&mut self) {
        if let Some(c) = self.client.take() {
            self.inner
                .clients
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(c);
        }
    }
}

impl Pool {
    /// Wait for a free client.
    pub async fn get(&self) -> PooledClient {
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .expect("the pool semaphore is never closed");
        let client = self
            .inner
            .clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop()
            .expect("a permit guarantees a free client");
        PooledClient {
            client: Some(client),
            inner: self.inner.clone(),
            _permit: permit,
        }
    }

    /// `host:port`, for `QF090` messages.
    pub fn target(&self) -> &str {
        &self.inner.target
    }

    /// Set the per-query timeout (default [`DEFAULT_QUERY_TIMEOUT`]).
    pub fn with_query_timeout(mut self, timeout: Duration) -> Self {
        self.query_timeout = timeout;
        self
    }

    pub fn query_timeout(&self) -> Duration {
        self.query_timeout
    }
}

/// Open `size` (at least 1) connections concurrently. Any failure, or not finishing within
/// `timeout`, is `QF090` "cannot reach database at host:port". A `connect_timeout` from the
/// connection (string, service or `PGCONNECT_TIMEOUT`) replaces `timeout`; as in libpq, less
/// than 2 seconds counts as 2. Needs a tokio runtime.
pub async fn connect_pool(
    c: &ResolvedConn,
    size: usize,
    timeout: Duration,
) -> Result<Pool, ConnError> {
    let size = size.max(1);
    let tls = tls_connector(c.ssl, c.root_cert.as_deref())?;
    let timeout = c
        .config
        .get_connect_timeout()
        .map_or(timeout, |t| (*t).max(Duration::from_secs(2)));
    let mut config = c.config.clone();
    config.connect_timeout(timeout);
    config.keepalives(true);
    config.keepalives_idle(KEEPALIVES_IDLE);
    let target = c.host_port.clone();

    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..size {
        let (config, tls) = (config.clone(), tls.clone());
        tasks.spawn(async move {
            let (client, connection) = config.connect(tls).await?;
            tokio::spawn(connection);
            Ok::<_, tokio_postgres::Error>(client)
        });
    }
    let all = async {
        let mut clients = Vec::with_capacity(size);
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok(Ok(client)) => clients.push(client),
                Ok(Err(e)) => return Err(ConnError::unreachable(&target, describe_pg_error(&e))),
                Err(e) => return Err(ConnError::unreachable(&target, e.to_string())),
            }
        }
        Ok(clients)
    };
    let clients = match tokio::time::timeout(timeout, all).await {
        Ok(r) => r?,
        Err(_) => {
            return Err(ConnError::unreachable(
                &target,
                format!("timed out after {}s", timeout.as_secs_f32()),
            ));
        }
    };
    Ok(Pool {
        inner: Arc::new(PoolInner {
            clients: Mutex::new(clients),
            target,
        }),
        permits: Arc::new(Semaphore::new(size)),
        query_timeout: DEFAULT_QUERY_TIMEOUT,
    })
}

/// A tokio-postgres error as text: the server's message when there is one, else the error chain.
pub(crate) fn describe_pg_error(e: &tokio_postgres::Error) -> String {
    if let Some(db) = e.as_db_error() {
        return db.message().to_string();
    }
    let mut s = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(cause) = source {
        let c = cause.to_string();
        if !s.contains(&c) {
            s.push_str(": ");
            s.push_str(&c);
        }
        source = cause.source();
    }
    s
}

#[cfg(test)]
#[path = "connect_tests.rs"]
mod tests;
