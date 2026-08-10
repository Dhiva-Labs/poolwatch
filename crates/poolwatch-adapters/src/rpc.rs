//! Shared HTTP/JSON-RPC plumbing: request helpers, response size guard,
//! HTTP digest auth (monerod --rpc-login), basic auth, cookie-file reload.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use md5::{Digest, Md5};
use poolwatch_core::AdapterError;
use reqwest::header::{HeaderMap, AUTHORIZATION, CONTENT_TYPE, WWW_AUTHENTICATE};
use reqwest::{Client, Response, StatusCode, Url};

/// Sent on every request so node operators can identify poolwatch in logs.
pub const USER_AGENT: &str = concat!("poolwatch/", env!("CARGO_PKG_VERSION"));

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Size guard for the small endpoints (stats, get_info, getblockchaininfo).
/// Only the full-pool fetch is worth a configurable limit.
pub(crate) const SMALL_RESPONSE_LIMIT: u64 = 4 * 1024 * 1024;

/// rustls needs exactly one process-wide crypto provider. We install `ring`
/// rather than the default `aws-lc-rs` because aws-lc-rs requires cmake and a
/// C toolchain, which breaks the static musl release build. TLS is rarely
/// exercised in practice (RPC is usually plain http on localhost) but must
/// keep working for users fronting their daemon with a TLS proxy.
fn install_crypto_provider() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        // Err means another component already installed a provider, which is
        // just as good for us; there is nothing to do and nothing to report.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn build_client() -> Result<Client, AdapterError> {
    install_crypto_provider();
    Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| AdapterError::Http(format!("building http client: {e}")))
}

/// How a request authenticates. One instance per adapter.
pub(crate) enum Auth {
    None,
    /// monerod `--rpc-login` (HTTP digest, RFC 2617).
    Digest(DigestAuth),
    /// bitcoind (HTTP basic, static credentials or a rotating cookie file).
    Basic(BasicAuth),
}

/// One shared client + auth state per adapter.
pub(crate) struct HttpClient {
    client: Client,
    auth: Auth,
}

impl HttpClient {
    pub(crate) fn new(auth: Auth) -> Result<Self, AdapterError> {
        Ok(Self {
            client: build_client()?,
            auth,
        })
    }

    /// POST `body` as JSON and return the raw response bytes, never buffering
    /// more than `limit`. Retries exactly once after a 401 (digest challenge
    /// or rotated cookie file).
    pub(crate) async fn post_json(
        &self,
        url: &str,
        body: &[u8],
        limit: u64,
    ) -> Result<Vec<u8>, AdapterError> {
        let mut retried = false;
        loop {
            let mut req = self
                .client
                .post(url)
                .header(CONTENT_TYPE, "application/json")
                .body(body.to_vec());

            match &self.auth {
                Auth::None => {}
                Auth::Digest(digest) => {
                    if let Some(header) = digest.authorization_header("POST", url)? {
                        req = req.header(AUTHORIZATION, header);
                    }
                }
                Auth::Basic(basic) => {
                    let (user, password) = basic.credentials()?;
                    req = req.basic_auth(user, Some(password));
                }
            }

            let response = req
                .send()
                .await
                .map_err(|e| AdapterError::Http(format!("{url}: {e}")))?;
            let status = response.status();

            if status == StatusCode::UNAUTHORIZED && !retried {
                match &self.auth {
                    Auth::None => {
                        return Err(AdapterError::Auth(format!(
                            "{url}: server requires authentication but none is configured"
                        )));
                    }
                    Auth::Digest(digest) => {
                        digest.learn_challenge(response.headers())?;
                        retried = true;
                        continue;
                    }
                    Auth::Basic(basic) => {
                        if basic.invalidate() {
                            retried = true;
                            continue;
                        }
                        return Err(AdapterError::Auth(format!("{url}: rejected credentials")));
                    }
                }
            }

            if let Some(err) = status_error(url, status) {
                return Err(err);
            }
            return read_limited(response, limit).await;
        }
    }
}

/// Map a non-2xx status onto the adapter error taxonomy. 404/405 are a
/// capability signal (daemon does not implement the endpoint), not a failure.
fn status_error(url: &str, status: StatusCode) -> Option<AdapterError> {
    if status.is_success() {
        return None;
    }
    match status {
        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED => {
            Some(AdapterError::Unsupported(format!("{url}: http {status}")))
        }
        StatusCode::UNAUTHORIZED => Some(AdapterError::Auth(format!(
            "{url}: http 401 after authentication attempt"
        ))),
        _ => Some(AdapterError::Http(format!("{url}: http {status}"))),
    }
}

/// Read a body, refusing to buffer more than `limit` bytes. Content-Length is
/// checked first (cheap, exact) but is only advisory: chunked responses do not
/// carry one, so the streamed length is enforced as well.
async fn read_limited(mut response: Response, limit: u64) -> Result<Vec<u8>, AdapterError> {
    if let Some(len) = response.content_length() {
        if len > limit {
            return Err(AdapterError::TooLarge { size: len, limit });
        }
    }

    let mut buf: Vec<u8> = Vec::new();
    loop {
        let chunk = response
            .chunk()
            .await
            .map_err(|e| AdapterError::Http(format!("reading body: {e}")))?;
        let Some(chunk) = chunk else { break };
        let size = buf.len() as u64 + chunk.len() as u64;
        if size > limit {
            // `size` is what we have seen so far, not the true body length —
            // we stop reading rather than pull the rest just to measure it.
            return Err(AdapterError::TooLarge { size, limit });
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

// ---------------------------------------------------------------------------
// HTTP basic auth (bitcoind): static user/pass, or a cookie file that the node
// rewrites on every restart.
// ---------------------------------------------------------------------------

pub(crate) enum BasicAuth {
    UserPass {
        user: String,
        password: String,
    },
    Cookie {
        path: PathBuf,
        cached: Mutex<Option<(String, String)>>,
    },
}

impl BasicAuth {
    pub(crate) fn cookie(path: PathBuf) -> Self {
        Self::Cookie {
            path,
            cached: Mutex::new(None),
        }
    }

    fn credentials(&self) -> Result<(String, String), AdapterError> {
        match self {
            Self::UserPass { user, password } => Ok((user.clone(), password.clone())),
            Self::Cookie { path, cached } => {
                if let Some(hit) = cached.lock().expect("cookie cache poisoned").clone() {
                    return Ok(hit);
                }
                let fresh = read_cookie_file(path)?;
                *cached.lock().expect("cookie cache poisoned") = Some(fresh.clone());
                Ok(fresh)
            }
        }
    }

    /// Drop cached credentials after a 401. Returns true if a retry can help,
    /// i.e. only for cookie files (they rotate when the node restarts).
    fn invalidate(&self) -> bool {
        match self {
            Self::UserPass { .. } => false,
            Self::Cookie { cached, .. } => {
                *cached.lock().expect("cookie cache poisoned") = None;
                true
            }
        }
    }
}

fn read_cookie_file(path: &Path) -> Result<(String, String), AdapterError> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| AdapterError::Io(format!("reading cookie file {}: {e}", path.display())))?;
    parse_cookie(&contents)
}

/// Parse a bitcoind `.cookie` file body: `__cookie__:<token>`. The token is
/// base64-ish and never contains a colon, so splitting on the first colon is
/// enough. The file has no trailing newline, but tolerate one.
pub fn parse_cookie(contents: &str) -> Result<(String, String), AdapterError> {
    let line = contents.trim_end_matches(['\r', '\n']);
    let (user, password) = line
        .split_once(':')
        .ok_or_else(|| AdapterError::Auth("cookie file has no ':' separator".to_string()))?;
    if user.is_empty() || password.is_empty() {
        return Err(AdapterError::Auth(
            "cookie file has an empty user or token".to_string(),
        ));
    }
    Ok((user.to_string(), password.to_string()))
}

// ---------------------------------------------------------------------------
// HTTP digest auth (RFC 2617). reqwest has no native digest support and
// monerod's --rpc-login speaks digest only.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigestAlgorithm {
    Md5,
    Md5Sess,
}

/// A parsed `WWW-Authenticate: Digest ...` challenge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestChallenge {
    pub realm: String,
    pub nonce: String,
    /// Selected qop, "auth" when the server offers it. `None` means the
    /// legacy RFC 2069 response form (no qop, no nc, no cnonce).
    pub qop: Option<String>,
    pub algorithm: DigestAlgorithm,
    pub opaque: Option<String>,
    pub stale: bool,
}

/// Everything the RFC 2617 response digest is computed from.
pub struct DigestInput<'a> {
    pub username: &'a str,
    pub password: &'a str,
    pub realm: &'a str,
    pub nonce: &'a str,
    /// Hex nonce count, zero-padded to 8 digits ("00000001").
    pub nc: &'a str,
    pub cnonce: &'a str,
    pub qop: Option<&'a str>,
    pub method: &'a str,
    /// Request-URI as sent on the request line (path plus query).
    pub uri: &'a str,
    pub algorithm: DigestAlgorithm,
}

fn md5_hex(parts: &[&str]) -> String {
    let mut hasher = Md5::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            hasher.update(b":");
        }
        hasher.update(part.as_bytes());
    }
    hex::encode(hasher.finalize())
}

/// RFC 2617 section 3.2.2.1 response digest. Pure — unit tested against the
/// RFC's worked example.
pub fn digest_response(input: &DigestInput<'_>) -> String {
    let mut ha1 = md5_hex(&[input.username, input.realm, input.password]);
    if input.algorithm == DigestAlgorithm::Md5Sess {
        ha1 = md5_hex(&[&ha1, input.nonce, input.cnonce]);
    }
    // qop=auth-int is never negotiated (we always select "auth"), so HA2 is
    // always the plain method:uri form.
    let ha2 = md5_hex(&[input.method, input.uri]);

    match input.qop {
        Some(qop) => md5_hex(&[&ha1, input.nonce, input.nc, input.cnonce, qop, &ha2]),
        None => md5_hex(&[&ha1, input.nonce, &ha2]),
    }
}

/// Parse a `WWW-Authenticate` header value and return the Digest challenge.
/// Servers may advertise several schemes; anything before the `Digest` token
/// is skipped. Returns `None` if no Digest challenge is present or if it is
/// missing realm/nonce.
pub fn parse_digest_challenge(header: &str) -> Option<DigestChallenge> {
    let rest = find_digest_scheme(header)?;
    let params = parse_auth_params(rest);

    let get = |name: &str| {
        params
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };

    let realm = get("realm")?;
    let nonce = get("nonce")?;

    // The server may offer a comma-separated list ("auth,auth-int"); we only
    // implement "auth". Anything else degrades to the RFC 2069 form.
    let qop = get("qop").and_then(|offered| {
        offered
            .split(',')
            .map(str::trim)
            .find(|candidate| candidate.eq_ignore_ascii_case("auth"))
            .map(|_| "auth".to_string())
    });

    let algorithm = match get("algorithm") {
        Some(value) if value.eq_ignore_ascii_case("MD5-sess") => DigestAlgorithm::Md5Sess,
        // Unknown algorithms (SHA-256 etc.) are not implemented; MD5 is what
        // monerod sends, so fall back to it rather than failing outright.
        _ => DigestAlgorithm::Md5,
    };

    Some(DigestChallenge {
        realm,
        nonce,
        qop,
        algorithm,
        opaque: get("opaque"),
        stale: get("stale").is_some_and(|v| v.eq_ignore_ascii_case("true")),
    })
}

/// Return the parameter section following the `Digest` scheme token.
fn find_digest_scheme(header: &str) -> Option<&str> {
    let mut haystack = header;
    loop {
        let at = haystack.to_ascii_lowercase().find("digest")?;
        let before_ok = at == 0
            || haystack[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c == ' ' || c == ',');
        let after = &haystack[at + "digest".len()..];
        if before_ok && after.starts_with(' ') {
            return Some(after.trim_start());
        }
        haystack = after;
    }
}

/// Split `key=value, key="value with, comma"` into pairs. Quoted values may
/// contain commas and backslash escapes, so a hand-rolled scan is needed.
fn parse_auth_params(input: &str) -> Vec<(String, String)> {
    let mut params = Vec::new();
    let mut chars = input.chars().peekable();

    loop {
        // key
        let mut key = String::new();
        while let Some(&c) = chars.peek() {
            if c == '=' || c == ',' {
                break;
            }
            key.push(c);
            chars.next();
        }
        let key = key.trim().to_string();

        let mut value = String::new();
        if chars.peek() == Some(&'=') {
            chars.next();
            if chars.peek() == Some(&'"') {
                chars.next();
                let mut escaped = false;
                for c in chars.by_ref() {
                    if escaped {
                        value.push(c);
                        escaped = false;
                    } else if c == '\\' {
                        escaped = true;
                    } else if c == '"' {
                        break;
                    } else {
                        value.push(c);
                    }
                }
            } else {
                while let Some(&c) = chars.peek() {
                    if c == ',' {
                        break;
                    }
                    value.push(c);
                    chars.next();
                }
            }
        }

        if !key.is_empty() {
            params.push((key, value.trim().to_string()));
        }

        // Skip the separator and any whitespace before the next key.
        match chars.peek() {
            Some(&',') => {
                chars.next();
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
            }
            Some(_) => {
                chars.next();
            }
            None => return params,
        }
    }
}

struct DigestState {
    challenge: Option<DigestChallenge>,
    nc: u32,
}

/// Digest credentials plus the challenge learned from the last 401. The nonce
/// is reused across requests (with an incrementing nc) until the server
/// answers 401 again, which is also how a `stale=true` nonce is handled.
pub(crate) struct DigestAuth {
    username: String,
    password: String,
    state: Mutex<DigestState>,
}

impl DigestAuth {
    /// Split a monerod `--rpc-login` value ("user:pass"; the password may
    /// itself contain colons, the username may not).
    pub(crate) fn from_login(login: &str) -> Result<Self, AdapterError> {
        let (username, password) = login.split_once(':').ok_or_else(|| {
            AdapterError::Auth("rpc_login must be in the form \"user:password\"".to_string())
        })?;
        if username.is_empty() {
            return Err(AdapterError::Auth(
                "rpc_login has an empty username".to_string(),
            ));
        }
        Ok(Self {
            username: username.to_string(),
            password: password.to_string(),
            state: Mutex::new(DigestState {
                challenge: None,
                nc: 0,
            }),
        })
    }

    /// Build an `Authorization` header from the cached challenge. `None` on
    /// the very first request, which is expected to draw a 401.
    fn authorization_header(
        &self,
        method: &str,
        url: &str,
    ) -> Result<Option<String>, AdapterError> {
        let (challenge, nc) = {
            let mut state = self.state.lock().expect("digest state poisoned");
            let Some(challenge) = state.challenge.clone() else {
                return Ok(None);
            };
            state.nc += 1;
            (challenge, state.nc)
        };

        let uri = request_uri(url)?;
        let nc = format!("{nc:08x}");
        let cnonce = hex::encode(rand::random::<[u8; 8]>());

        let response = digest_response(&DigestInput {
            username: &self.username,
            password: &self.password,
            realm: &challenge.realm,
            nonce: &challenge.nonce,
            nc: &nc,
            cnonce: &cnonce,
            qop: challenge.qop.as_deref(),
            method,
            uri: &uri,
            algorithm: challenge.algorithm,
        });

        let mut header = format!(
            "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", response=\"{}\"",
            self.username, challenge.realm, challenge.nonce, uri, response
        );
        header.push_str(match challenge.algorithm {
            DigestAlgorithm::Md5 => ", algorithm=MD5",
            DigestAlgorithm::Md5Sess => ", algorithm=MD5-sess",
        });
        if let Some(qop) = &challenge.qop {
            header.push_str(&format!(", qop={qop}, nc={nc}, cnonce=\"{cnonce}\""));
        }
        if let Some(opaque) = &challenge.opaque {
            header.push_str(&format!(", opaque=\"{opaque}\""));
        }
        Ok(Some(header))
    }

    /// Record the challenge from a 401 response and restart the nc counter.
    fn learn_challenge(&self, headers: &HeaderMap) -> Result<(), AdapterError> {
        let raw = headers
            .get(WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                AdapterError::Auth("401 without a WWW-Authenticate header".to_string())
            })?;
        let challenge = parse_digest_challenge(raw).ok_or_else(|| {
            AdapterError::Auth(format!("401 without a usable Digest challenge: {raw}"))
        })?;
        if challenge.stale {
            tracing::debug!("digest nonce went stale, retrying with the refreshed challenge");
        }
        let mut state = self.state.lock().expect("digest state poisoned");
        state.challenge = Some(challenge);
        state.nc = 0;
        Ok(())
    }
}

/// The digest URI is the request target from the request line: path plus
/// query, not the absolute URL.
fn request_uri(url: &str) -> Result<String, AdapterError> {
    let parsed = Url::parse(url).map_err(|e| AdapterError::Http(format!("bad url {url}: {e}")))?;
    Ok(match parsed.query() {
        Some(query) => format!("{}?{}", parsed.path(), query),
        None => parsed.path().to_string(),
    })
}

/// Join a configured base URL and an endpoint path, tolerating a trailing
/// slash on the base.
pub(crate) fn join_url(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

/// Deserialize a response body, turning serde errors into `Parse`.
pub(crate) fn parse_json<'a, T: serde::Deserialize<'a>>(body: &'a [u8]) -> Result<T, AdapterError> {
    serde_json::from_slice(body).map_err(|e| AdapterError::Parse(e.to_string()))
}

/// Transactions plus the pool byte total derived from one full-pool response.
#[derive(Debug, Clone)]
pub struct ParsedPool {
    pub txs: Vec<poolwatch_core::PoolTx>,
    pub pool_bytes: u64,
}

/// Node identity and height, read from a daemon's info endpoint(s).
#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub height: u64,
    pub version: String,
    pub peer_count: Option<u32>,
}

/// A JSON-RPC 2.0 (monerod /json_rpc) or 1.0 (bitcoind) response envelope.
/// Both families use the same three fields; only `id` handling differs and we
/// never inspect it.
#[derive(serde::Deserialize)]
pub(crate) struct JsonRpcEnvelope<T> {
    // Option fields are implicitly optional in serde; an explicit
    // `#[serde(default)]` here would demand `T: Default`.
    pub(crate) result: Option<T>,
    pub(crate) error: Option<JsonRpcError>,
}

#[derive(serde::Deserialize)]
pub(crate) struct JsonRpcError {
    pub(crate) code: i64,
    pub(crate) message: String,
}

/// Unwrap a JSON-RPC envelope, mapping an `error` object onto
/// [`AdapterError::Rpc`].
pub(crate) fn jsonrpc_result<T: serde::de::DeserializeOwned>(
    body: &[u8],
) -> Result<T, AdapterError> {
    let envelope: JsonRpcEnvelope<T> = parse_json(body)?;
    if let Some(error) = envelope.error {
        return Err(AdapterError::Rpc {
            code: error.code,
            message: error.message,
        });
    }
    envelope
        .result
        .ok_or_else(|| AdapterError::Parse("json-rpc response has no result".to_string()))
}
