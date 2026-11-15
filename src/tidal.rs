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
    /// The transport or the API returned a non-success status.
    Http(String),
    /// The response body did not match the expected shape.
    Decode(String),
    /// The resource does not exist.
    NotFound,
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
        HttpService { client: reqwest::blocking::Client::new(), base: base.trim_end_matches('/').to_string() }
    }

    fn get_doc(&self, token: &str, path: &str) -> Result<Document, ServiceError> {
        let url = format!("{}{}", self.base, path);
        let resp = self
            .client
            .get(&url)
            .bearer_auth(token)
            .header("accept", "application/vnd.tidal.v1+json")
            .send()
            .map_err(|e| ServiceError::Http(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(ServiceError::Http(format!("{status} for {path}")));
        }
        resp.json::<Document>().map_err(|e| ServiceError::Decode(e.to_string()))
    }
}

impl MusicService for HttpService {
    fn current_user(&self, token: &str) -> Result<User, ServiceError> {
        let doc = self.get_doc(token, "/users/me")?;
        let data = doc.data.ok_or(ServiceError::NotFound)?;
        Ok(User { id: UserId(data.id), name: name_from(&data.attributes) })
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
    token_from_response(resp)
}

fn token_from_response(resp: reqwest::blocking::Response) -> Result<Token, ServiceError> {
    let status = resp.status();
    let body = resp.text().map_err(|e| ServiceError::Http(e.to_string()))?;
    if !status.is_success() {
        return Err(ServiceError::Auth(format!("{status}: {body}")));
    }
    crate::auth::parse_token(&body, "").map_err(ServiceError::Decode)
}

/// The loopback half of the sign-in: bind the redirect port, read the one
/// callback, check the state, and exchange the code. Blocking, so it runs on a
/// `Latest` worker thread while the browser is open.
pub fn run_loopback(config: OAuthConfig, state: String, verifier: String) -> Result<Token, String> {
    use std::io::Read;
    let addr = loopback_addr(&config.redirect_uri)?;
    let listener = std::net::TcpListener::bind(&addr).map_err(|e| e.to_string())?;
    let (mut stream, _) = listener.accept().map_err(|e| e.to_string())?;
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).map_err(|e| e.to_string())?;
    let request = String::from_utf8_lossy(&buf[..n]);
    let Some((code, got_state)) = parse_callback(&request) else {
        return Err("no code in the redirect".to_string());
    };
    if got_state != state {
        return Err("state mismatch".to_string());
    }
    let client = reqwest::blocking::Client::new();
    exchange_code(&client, &config, &code, &verifier).map_err(|e| format!("{e:?}"))
}

fn loopback_addr(redirect_uri: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(redirect_uri).map_err(|e| e.to_string())?;
    let host = url.host_str().ok_or("redirect_uri has no host")?;
    let port = url.port().ok_or("redirect_uri has no port")?;
    Ok(format!("{host}:{port}"))
}

/// Pull `code` and `state` out of the callback request line
/// (`GET /callback?code=..&state=.. HTTP/1.1`).
fn parse_callback(request: &str) -> Option<(String, String)> {
    let line = request.lines().next()?;
    let mut parts = line.split_whitespace();
    let _method = parts.next()?;
    let target = parts.next()?;
    let query = target.split('?').nth(1)?;
    let mut code = None;
    let mut state = None;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        match key {
            "code" => code = Some(value.to_string()),
            "state" => state = Some(value.to_string()),
            _ => {}
        }
    }
    Some((code?, state?))
}

/// The JSON:API document envelope. Tidal wraps every resource in `data` with
/// `id`, `type` and `attributes`.
#[derive(Deserialize)]
struct Document {
    data: Option<Resource>,
}

#[derive(Deserialize)]
struct Resource {
    id: String,
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
}
