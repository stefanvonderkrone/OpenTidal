//! The Tidal boundary: the remote data types, the [`MusicService`] port, and
//! its two impls. [`FakeService`] is deterministic and backs every proof and
//! headless drive. [`HttpService`] is a thin blocking `reqwest` client over the
//! Tidal JSON:API.
//!
//! This module is rune-free. It holds no network or token state in `State`; the
//! app wraps a service in `Arc` on [`crate::app::App`].

use serde::Deserialize;
use serde_json::Value;

use crate::auth::{OAuthConfig, Token};

/// A Tidal id. Tidal ids are opaque strings, not the contiguous `u8` ids the
/// placeholder catalogue used. The newtype keeps an id from being mistaken for
/// any other string.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct UserId(pub String);

/// The authenticated user's profile, the first remote entity.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct User {
    pub id: UserId,
    pub name: String,
}

/// A failure from the service, named so a test reports what broke.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ServiceError {
    /// The auth endpoints rejected the exchange or refresh.
    Auth(String),
    /// The token was rejected as expired or invalid; refresh and retry.
    Unauthorized,
    /// The transport or the API returned a non-success status.
    Http(String),
    /// The response body did not match the expected shape.
    Decode(String),
}

/// The read-only music port. Every method takes the access token, so the fake
/// ignores it and the real client sends it as a bearer header.
pub trait MusicService: Send + Sync {
    fn current_user(&self, token: &str) -> Result<User, ServiceError>;
}

/// A fixed, deterministic service for tests and headless drives.
pub struct FakeService {
    pub user: User,
}

impl Default for FakeService {
    fn default() -> Self {
        FakeService { user: User { id: UserId("fake-user".to_string()), name: "Fake User".to_string() } }
    }
}

impl MusicService for FakeService {
    fn current_user(&self, _token: &str) -> Result<User, ServiceError> {
        Ok(self.user.clone())
    }
}

/// The real client over the Tidal JSON:API (`https://openapi.tidal.com/v2`).
pub struct HttpService {
    client: reqwest::blocking::Client,
    base: String,
}

impl HttpService {
    pub fn new(base: &str) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("http client");
        HttpService { client, base: base.trim_end_matches('/').to_string() }
    }

    fn get_doc(&self, token: &str, path: &str) -> Result<Document, ServiceError> {
        let url = format!("{}{}", self.base, path);
        let resp = self
            .client
            .get(&url)
            .bearer_auth(token)
            .header("accept", "application/vnd.api+json")
            .send()
            .map_err(|e| ServiceError::Http(e.to_string()))?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ServiceError::Unauthorized);
        }
        if !status.is_success() {
            return Err(ServiceError::Http(format!("{status} for {path}")));
        }
        resp.json::<Document>().map_err(|e| ServiceError::Decode(e.to_string()))
    }
}

impl MusicService for HttpService {
    /// The user id comes from the token's `uid` claim, not from `/users/me`,
    /// which returns 404 for many third-party clients. The name comes from the
    /// token when it carries one, else from `/users/{id}`.
    fn current_user(&self, token: &str) -> Result<User, ServiceError> {
        let claims = crate::auth::decode_claims(token)
            .ok_or_else(|| ServiceError::Decode("token is not a JWT".to_string()))?;
        let id = claims.user_id().ok_or_else(|| ServiceError::Decode("token has no uid claim".to_string()))?;
        let mut name = claims.display_name().unwrap_or_else(|| "You".to_string());
        if name == "You" {
            if let Ok(doc) = self.get_doc(token, &format!("/users/{id}")) {
                if let Some(data) = doc.data {
                    let profile = name_from(&data.attributes);
                    if profile != "You" {
                        name = profile;
                    }
                }
            }
        }
        Ok(User { id: UserId(id), name })
    }
}

/// Exchange an authorization code for a token (authorization code + PKCE).
pub fn exchange_code(
    client: &reqwest::blocking::Client,
    config: &OAuthConfig,
    code: &str,
    verifier: &str,
) -> Result<Token, ServiceError> {
    let resp = client
        .post(&config.token_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", config.client_id.as_str()),
            ("code", code),
            ("redirect_uri", config.redirect_uri.as_str()),
            ("code_verifier", verifier),
        ])
        .send()
        .map_err(|e| ServiceError::Http(e.to_string()))?;
    token_from_response(resp, "")
}

fn token_from_response(resp: reqwest::blocking::Response, fallback_refresh: &str) -> Result<Token, ServiceError> {
    let status = resp.status();
    let body = resp.text().map_err(|e| ServiceError::Http(e.to_string()))?;
    if !status.is_success() {
        return Err(ServiceError::Auth(format!("{status}: {body}")));
    }
    crate::auth::parse_token(&body, fallback_refresh).map_err(ServiceError::Decode)
}

/// Exchange a refresh token for a fresh access token (refresh grant). The old
/// refresh token fills in when the response omits one.
pub fn refresh_token(
    client: &reqwest::blocking::Client,
    config: &OAuthConfig,
    refresh_token: &str,
) -> Result<Token, ServiceError> {
    let resp = client
        .post(&config.token_url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", config.client_id.as_str()),
            ("refresh_token", refresh_token),
        ])
        .send()
        .map_err(|e| ServiceError::Http(e.to_string()))?;
    token_from_response(resp, refresh_token)
}

/// Bind the loopback port from the redirect URI and return the listener, ready
/// to accept, before the browser is asked to redirect to it. Binding first
/// removes the race where the browser redirects to a port no one listens on.
pub fn bind_loopback(redirect_uri: &str) -> Result<std::net::TcpListener, String> {
    let port = loopback_port(redirect_uri)?;
    let addr = format!("127.0.0.1:{port}");
    std::net::TcpListener::bind(&addr).map_err(|e| {
        format!("cannot listen on {addr}: {e}. Check the port is free and the URI matches the dashboard")
    })
}

/// How long the loopback listener waits for the browser's callback before it
/// gives up, so closing the browser cannot leave sign-in pending forever.
const SIGN_IN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// The loopback half of the sign-in: accept one callback on the already-bound
/// listener, check the state, exchange the code, and answer the browser.
/// Blocking, so it runs on a `Latest` worker thread while the browser is open.
pub fn run_loopback(
    config: OAuthConfig,
    state: String,
    verifier: String,
    listener: std::net::TcpListener,
) -> Result<Token, String> {
    use std::io::{Read, Write};
    let mut stream = accept_with_timeout(&listener)?;
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).map_err(|e| e.to_string())?;
    let request = String::from_utf8_lossy(&buf[..n]).to_string();
    let result = exchange_from_callback(&request, &config, &state, &verifier);
    // Answer the browser on every path so it is not left waiting.
    let (status, body) = match &result {
        Ok(_) => ("200 OK", "You can close this tab and return to OpenTidal."),
        Err(e) => ("400 Bad Request", e.as_str()),
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
    result
}

/// Accept one connection within [`SIGN_IN_TIMEOUT`], or fail so the caller can
/// leave the `Authorizing` state instead of blocking forever.
fn accept_with_timeout(listener: &std::net::TcpListener) -> Result<std::net::TcpStream, String> {
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let deadline = std::time::Instant::now() + SIGN_IN_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nonblocking(false);
                return Ok(stream);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    return Err("sign-in timed out: close the browser and try again".to_string());
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Parse and validate one callback, then exchange its code for a token.
fn exchange_from_callback(
    request: &str,
    config: &OAuthConfig,
    state: &str,
    verifier: &str,
) -> Result<Token, String> {
    let Some(callback) = parse_callback(request) else {
        return Err("the browser did not return a code or error".to_string());
    };
    if let Some(error) = callback.error {
        let desc = callback.error_description.as_deref().unwrap_or("");
        return Err(format!("Tidal rejected the login: {error} ({desc})"));
    }
    let code = callback.code.ok_or("no code in the redirect")?;
    if callback.state.as_deref() != Some(state) {
        return Err("state mismatch".to_string());
    }
    let client = reqwest::blocking::Client::new();
    exchange_code(&client, config, &code, verifier).map_err(|e| format!("{e:?}"))
}

/// The port from the redirect URI. The listener binds `127.0.0.1` regardless of
/// whether the URI names `localhost` or the loopback IP, so a `localhost` URI
/// cannot bind `::1` and miss the browser's IPv4 connection.
fn loopback_port(redirect_uri: &str) -> Result<u16, String> {
    let url = reqwest::Url::parse(redirect_uri).map_err(|e| e.to_string())?;
    url.port().ok_or_else(|| "redirect_uri has no port".to_string())
}

struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// Pull the OAuth params out of the callback request line
/// (`GET /callback?code=..&state=.. HTTP/1.1`), or the error Tidal sent instead.
/// Values are percent-decoded, so `code=a%2Bb` reads as `a+b`.
fn parse_callback(request: &str) -> Option<Callback> {
    let line = request.lines().next()?;
    let mut parts = line.split_whitespace();
    let _method = parts.next()?;
    let target = parts.next()?;
    let url = reqwest::Url::parse(&format!("http://localhost{target}")).ok()?;
    let mut cb = Callback { code: None, state: None, error: None, error_description: None };
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => cb.code = Some(value.into_owned()),
            "state" => cb.state = Some(value.into_owned()),
            "error" => cb.error = Some(value.into_owned()),
            "error_description" => cb.error_description = Some(value.into_owned()),
            _ => {}
        }
    }
    Some(cb)
}

/// The JSON:API document envelope. Tidal wraps every resource in `data`;
/// here only its `attributes` are read, so the other members are ignored.
#[derive(Deserialize)]
struct Document {
    data: Option<Resource>,
}

#[derive(Deserialize)]
struct Resource {
    attributes: Option<Value>,
}

/// The user's display name from the resource attributes. The field names vary
/// across the API versions, so read the known ones in order and fall back.
fn name_from(attributes: &Option<Value>) -> String {
    let Some(attrs) = attributes else { return "You".to_string() };
    let mut parts = Vec::new();
    if let Some(f) = attrs.get("firstName").and_then(Value::as_str) {
        parts.push(f.to_string());
    }
    if let Some(l) = attrs.get("lastName").and_then(Value::as_str) {
        parts.push(l.to_string());
    }
    if !parts.is_empty() {
        return parts.join(" ");
    }
    for key in ["name", "username"] {
        if let Some(s) = attrs.get(key).and_then(Value::as_str) {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    "You".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fake returns its fixed user and never touches the network.
    #[test]
    fn fake_service_returns_its_user() {
        let svc = FakeService::default();
        let u = svc.current_user("any-token").unwrap();
        assert_eq!(u.name, "Fake User");
        assert_eq!(u.id, UserId("fake-user".to_string()));
    }

    /// A name comes from the first present field, in order.
    #[test]
    fn name_prefers_first_and_last() {
        let attrs = serde_json::json!({ "firstName": "Ada", "lastName": "Lovelace" });
        assert_eq!(name_from(&Some(attrs)), "Ada Lovelace");
    }

    #[test]
    fn name_falls_back_to_username_then_you() {
        let attrs = serde_json::json!({ "username": "ada" });
        assert_eq!(name_from(&Some(attrs)), "ada");
        assert_eq!(name_from(&None), "You");
    }

    /// The loopback port comes from the URI, and a URI without a port fails.
    #[test]
    fn loopback_port_is_read_from_the_uri() {
        assert_eq!(loopback_port("http://127.0.0.1:8080/callback").unwrap(), 8080);
        assert_eq!(loopback_port("http://localhost:8080/callback").unwrap(), 8080);
        assert!(loopback_port("http://127.0.0.1/callback").is_err());
    }

    /// The callback parser reads a code, and a rejection with its reason.
    #[test]
    fn callback_parser_reads_code_and_error() {
        let ok = parse_callback("GET /callback?code=abc&state=st HTTP/1.1").unwrap();
        assert_eq!(ok.code.as_deref(), Some("abc"));
        assert_eq!(ok.state.as_deref(), Some("st"));
        assert_eq!(ok.error, None);

        let err = parse_callback("GET /callback?error=access_denied&error_description=nope&state=st HTTP/1.1").unwrap();
        assert_eq!(err.error.as_deref(), Some("access_denied"));
        assert_eq!(err.error_description.as_deref(), Some("nope"));
    }

    /// Percent-encoded callback values are decoded before use.
    #[test]
    fn callback_parser_decodes_values() {
        let cb = parse_callback("GET /callback?code=a%2Bb&state=%2F%3D HTTP/1.1").unwrap();
        assert_eq!(cb.code.as_deref(), Some("a+b"));
        assert_eq!(cb.state.as_deref(), Some("/="));
    }
}
