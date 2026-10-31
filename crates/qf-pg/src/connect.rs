//! Connection resolution (explicit, env var, standard env, helper command), TLS and the pool.
//!
//! Connection strings are never logged or written: [`ConnSource`], [`ResolvedConn`] and
//! [`ConnError`] never show a password or a resolved connection string.

use std::fmt;
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

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

const PG_VARS: [&str; 6] = [
    "PGHOST",
    "PGPORT",
    "PGUSER",
    "PGDATABASE",
    "PGPASSWORD",
    "PGSSLMODE",
];

/// Resolve the first source that yields a connection string. A helper command that fails, or a
/// string that does not parse, is a `QF091` error rather than a fall-through.
pub fn resolve_connection(sources: &[ConnSource]) -> Result<ResolvedConn, ConnError> {
    for source in sources {
        match source {
            ConnSource::Explicit(s) if !s.trim().is_empty() => {
                return parse_conn(s.trim(), "the explicit connection string");
            }
            ConnSource::Explicit(_) => {}
            ConnSource::Env(name) => {
                if let Some(v) = env_nonempty(name) {
                    return parse_conn(v.trim(), &format!("environment variable {name}"));
                }
            }
            ConnSource::StandardEnv => {
                if let Some(v) = env_nonempty("DATABASE_URL") {
                    return parse_conn(v.trim(), "environment variable DATABASE_URL");
                }
                if PG_VARS[..5].iter().any(|v| env_nonempty(v).is_some()) {
                    return parse_conn("", "the PG* environment variables");
                }
            }
            ConnSource::Command(cmd) => {
                let out = run_helper(cmd)?;
                return parse_conn(&out, &format!("the output of `{}`", redact(cmd)));
            }
        }
    }
    Err(
        ConnError::config("no database connection configured").with_detail(
            "pass a connection string, set DATABASE_URL or the PG* variables, \
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

/// The TLS options Queryfirst applies itself: tokio-postgres knows only disable, prefer and
/// require, and has no `sslrootcert`.
#[derive(Debug, Default)]
struct TlsOptions {
    mode: Option<SslMode>,
    root_cert: Option<String>,
}

impl TlsOptions {
    /// Record `key=value` if it is one of ours; `false` for any other option.
    fn take(&mut self, key: &str, value: &str) -> Result<bool, ConnError> {
        match key {
            "sslmode" => self.mode = Some(parse_sslmode(value)?),
            "sslrootcert" => self.root_cert = Some(value.to_string()),
            _ => return Ok(false),
        }
        Ok(true)
    }
}

/// Take `sslmode` and `sslrootcert` out of a URL or `key=value` connection string, returning the
/// string without them; they are applied after parsing. Only real options count: never text
/// inside a password or another value.
fn take_tls_options(s: &str) -> Result<(String, TlsOptions), ConnError> {
    if s.starts_with("postgres://") || s.starts_with("postgresql://") {
        take_tls_options_url(s)
    } else {
        take_tls_options_kv(s)
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

fn take_tls_options_url(s: &str) -> Result<(String, TlsOptions), ConnError> {
    // The authority (userinfo and hosts) runs from `://` to the first `/`, `?` or `#`; the query
    // starts at the first `?` after it, so an `@` in a query value can never hide an option.
    let authority = s.find("://").map_or(0, |i| i + 3);
    let path = s[authority..]
        .find(['/', '?', '#'])
        .map_or(s.len(), |i| authority + i);
    let mut opts = TlsOptions::default();
    let Some(q) = s[path..].find('?').map(|i| path + i) else {
        return Ok((s.to_string(), opts));
    };
    let mut kept = Vec::new();
    for pair in s[q + 1..].split('&') {
        match pair.split_once('=') {
            Some((k, v)) if opts.take(&percent_decode(k), &percent_decode(v))? => {}
            // tokio-postgres reads the userinfo up to the first `@` anywhere in the URL: escape
            // the ones in the query (it decodes them back).
            Some(_) => kept.push(pair.replace('@', "%40")),
            None => kept.push(pair.to_string()),
        }
    }
    let base = &s[..q];
    let rebuilt = if kept.iter().all(|p| p.is_empty()) {
        base.to_string()
    } else {
        format!("{base}?{}", kept.join("&"))
    };
    Ok((rebuilt, opts))
}

/// libpq `key=value` form: whitespace-separated, spaces allowed around `=`, values optionally
/// single-quoted, backslash escapes in both.
fn take_tls_options_kv(s: &str) -> Result<(String, TlsOptions), ConnError> {
    let b: Vec<(usize, char)> = s.char_indices().collect();
    let at = |i: usize| b.get(i).map(|&(_, c)| c);
    let byte = |i: usize| b.get(i).map_or(s.len(), |&(o, _)| o);
    let skip_ws = |mut i: usize| {
        while at(i).is_some_and(char::is_whitespace) {
            i += 1;
        }
        i
    };
    let mut opts = TlsOptions::default();
    let mut out = String::new();
    let mut i = 0;
    loop {
        let token_start = skip_ws(i);
        if token_start >= b.len() {
            out.push_str(&s[byte(i)..]);
            break;
        }
        let mut j = token_start;
        while at(j).is_some_and(|c| c != '=' && !c.is_whitespace()) {
            j += 1;
        }
        let key = &s[byte(token_start)..byte(j)];
        j = skip_ws(j);
        if at(j) != Some('=') {
            // Malformed; leave the rest for tokio-postgres to reject.
            out.push_str(&s[byte(i)..]);
            break;
        }
        j = skip_ws(j + 1);
        let mut value = String::new();
        if at(j) == Some('\'') {
            j += 1;
            loop {
                match at(j) {
                    None => break,
                    Some('\'') => {
                        j += 1;
                        break;
                    }
                    Some('\\') if let Some(next) = at(j + 1) => {
                        value.push(next);
                        j += 2;
                    }
                    Some(c) => {
                        value.push(c);
                        j += 1;
                    }
                }
            }
        } else {
            while let Some(c) = at(j).filter(|c| !c.is_whitespace()) {
                if let Some(next) = at(j + 1).filter(|_| c == '\\') {
                    value.push(next);
                    j += 2;
                } else {
                    value.push(c);
                    j += 1;
                }
            }
        }
        if !opts.take(key, &value)? {
            out.push_str(&s[byte(i)..byte(j)]);
        }
        i = j;
    }
    Ok((out.trim().to_string(), opts))
}

/// The libpq options Queryfirst accepts (tokio-postgres' list, plus `sslrootcert`).
const SUPPORTED_OPTIONS: &str = "host, hostaddr, port, user, password, dbname, options, \
     application_name, connect_timeout, tcp_user_timeout, keepalives, keepalives_idle, \
     keepalives_interval, keepalives_retries, target_session_attrs, channel_binding, \
     load_balance_hosts, sslnegotiation, sslmode and sslrootcert";

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

fn parse_conn(s: &str, origin: &str) -> Result<ResolvedConn, ConnError> {
    let (s, tls) = take_tls_options(s)?;
    let mut config = if s.is_empty() {
        tokio_postgres::Config::new()
    } else {
        tokio_postgres::Config::from_str(&s).map_err(|e| invalid_string(origin, &e))?
    };

    // Fill what the string left out from the libpq environment, as libpq does.
    if config.get_hosts().is_empty() {
        let hosts = env_nonempty("PGHOST").unwrap_or_else(|| "localhost".into());
        for h in hosts.split(',') {
            config.host(h.trim());
        }
    }
    if config.get_ports().is_empty() {
        let port = match env_nonempty("PGPORT") {
            Some(p) => p
                .trim()
                .parse::<u16>()
                .map_err(|_| ConnError::config("invalid PGPORT"))?,
            None => 5432,
        };
        config.port(port);
    }
    if config.get_user().is_none() {
        let user = env_nonempty("PGUSER")
            .or_else(|| env_nonempty("USER"))
            .or_else(|| env_nonempty("USERNAME"))
            .unwrap_or_else(|| "postgres".into());
        config.user(user);
    }
    if config.get_dbname().is_none() {
        let db = env_nonempty("PGDATABASE")
            .unwrap_or_else(|| config.get_user().unwrap_or_default().to_string());
        config.dbname(db);
    }
    if config.get_password().is_none()
        && let Some(pw) = env_nonempty("PGPASSWORD")
    {
        config.password(pw);
    }

    let ssl = match tls.mode {
        Some(m) => m,
        // A mode tokio-postgres parsed itself (only if ours missed it) is still the string's,
        // which beats the environment: never silently weaken it to `prefer`.
        None => match config.get_ssl_mode() {
            tokio_postgres::config::SslMode::Disable => SslMode::Disable,
            tokio_postgres::config::SslMode::Require => SslMode::Require,
            _ => match env_nonempty("PGSSLMODE") {
                Some(v) => SslMode::parse(v.trim()).ok_or_else(|| {
                    ConnError::config(format!("invalid PGSSLMODE `{}`", v.trim()))
                })?,
                None => SslMode::Prefer,
            },
        },
    };
    config.ssl_mode(match ssl {
        SslMode::Disable => tokio_postgres::config::SslMode::Disable,
        SslMode::Prefer => tokio_postgres::config::SslMode::Prefer,
        _ => tokio_postgres::config::SslMode::Require,
    });
    let root_cert = tls.root_cert.or_else(|| env_nonempty("PGSSLROOTCERT"));

    let host = match &config.get_hosts()[0] {
        Host::Tcp(h) => h.clone(),
        #[cfg(unix)]
        Host::Unix(p) => p.display().to_string(),
    };
    let port = config.get_ports()[0];
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
/// `timeout`, is `QF090` "cannot reach database at host:port". Needs a tokio runtime.
pub async fn connect_pool(
    c: &ResolvedConn,
    size: usize,
    timeout: Duration,
) -> Result<Pool, ConnError> {
    let size = size.max(1);
    let tls = tls_connector(c.ssl, c.root_cert.as_deref())?;
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
