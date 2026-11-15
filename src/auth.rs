//! The OAuth 2.1 authorization-code flow with PKCE (S256), the auth state
//! machine, and the token store. Rune-free: nothing here depends on the view.
//! The HTTP exchange lives in [`crate::tidal`]; this module is the pure state,
//! the crypto, and the parsing, so a test proves it without a network.

use std::sync::Mutex;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// An OAuth access token and its refresh token.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Token {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: u64,
}

/// The client registration the operator supplies out of band (env or a
/// gitignored config file). The secret never enters the repo.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub scope: String,
    pub authorize_url: String,
    pub token_url: String,
}

impl OAuthConfig {
    /// The production Tidal endpoints. Credentials are filled from env by the
    /// caller.
    pub fn tidal(client_id: String, client_secret: String, redirect_uri: String, scope: String) -> Self {
        OAuthConfig {
            client_id,
            client_secret,
            redirect_uri,
            scope,
            authorize_url: "https://login.tidal.com/authorize".to_string(),
            token_url: "https://auth.tidal.com/v1/oauth2/token".to_string(),
        }
    }
}

/// The auth state. A variant carries only the data valid in it: a login in
/// flight holds its verifier and state, a signed-in state holds a token.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Auth {
    SignedOut,
    Authorizing { state: String, verifier: String },
    SignedIn { token: Token, name: String },
    Failed { message: String },
}

impl Default for Auth {
    fn default() -> Self {
        Auth::SignedOut
    }
}

impl Auth {
    /// The displayed name, or `None` while signed out.
    pub fn name(&self) -> Option<&str> {
        match self {
            Auth::SignedIn { name, .. } => Some(name),
            _ => None,
        }
    }
}

/// A random PKCE code verifier: 32 bytes, base64url, no padding.
pub fn generate_verifier() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The S256 code challenge for a verifier: base64url(sha256(verifier)).
pub fn code_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// A random state to bind the redirect to the login that started it.
pub fn generate_state() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The authorize URL the browser opens, with a fresh verifier and state.
pub fn begin(config: &OAuthConfig) -> (String, String, String) {
    let verifier = generate_verifier();
    let state = generate_state();
    let url = authorize_url(config, &verifier, &state);
    (url, verifier, state)
}

fn authorize_url(config: &OAuthConfig, verifier: &str, state: &str) -> String {
    let mut url = reqwest::Url::parse(&config.authorize_url).expect("valid authorize url");
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &config.client_id)
        .append_pair("redirect_uri", &config.redirect_uri)
        .append_pair("scope", &config.scope)
        .append_pair("code_challenge_method", "S256")
        .append_pair("code_challenge", &code_challenge(verifier))
        .append_pair("state", state);
    url.to_string()
}

/// A token endpoint body, parsed with the refresh token carried over when the
/// response omits it (the refresh flow returns no new refresh token).
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: u64,
}

/// Parse a token endpoint response. `fallback_refresh` fills a missing refresh
/// token, as the refresh flow omits one.
pub fn parse_token(json: &str, fallback_refresh: &str) -> Result<Token, String> {
    let tr: TokenResponse = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(Token {
        access_token: tr.access_token,
        refresh_token: tr.refresh_token.unwrap_or_else(|| fallback_refresh.to_string()),
        expires_in: tr.expires_in,
    })
}

/// The claims of a Tidal access token, which is a JWT. The `sub` claim is the
/// user id; the name fields vary, so read them in order.
#[derive(Default, Deserialize)]
pub struct JwtClaims {
    #[serde(default)]
    pub sub: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

impl JwtClaims {
    /// A display name from the token, in order of preference.
    pub fn display_name(&self) -> Option<String> {
        [&self.username, &self.name, &self.email]
            .into_iter()
            .flatten()
            .map(|s| s.trim().to_string())
            .find(|s| !s.is_empty())
    }
}

/// Decode the payload of a JWT access token. Returns `None` when the token is
/// not a JWT or its payload is not the expected shape.
pub fn decode_claims(token: &str) -> Option<JwtClaims> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Where the refresh token persists between runs.
pub trait TokenStore: Send + Sync {
    fn load(&self) -> Option<Token>;
    fn save(&self, token: &Token);
    fn clear(&self);
}

/// An in-memory store for tests and non-macOS platforms.
#[derive(Default)]
pub struct MemoryStore {
    token: Mutex<Option<Token>>,
}

impl TokenStore for MemoryStore {
    fn load(&self) -> Option<Token> {
        self.token.lock().unwrap().clone()
    }
    fn save(&self, token: &Token) {
        *self.token.lock().unwrap() = Some(token.clone());
    }
    fn clear(&self) {
        *self.token.lock().unwrap() = None;
    }
}

/// The macOS keychain, holding the token as JSON under one entry.
#[cfg(target_os = "macos")]
pub struct KeychainStore {
    entry: keyring::Entry,
}

#[cfg(target_os = "macos")]
impl KeychainStore {
    pub fn new(service: &str, account: &str) -> Self {
        KeychainStore { entry: keyring::Entry::new(service, account).expect("keychain entry") }
    }
}

#[cfg(target_os = "macos")]
impl TokenStore for KeychainStore {
    fn load(&self) -> Option<Token> {
        let json = self.entry.get_password().ok()?;
        serde_json::from_str(&json).ok()
    }
    fn save(&self, token: &Token) {
        if let Ok(json) = serde_json::to_string(token) {
            let _ = self.entry.set_password(&json);
        }
    }
    fn clear(&self) {
        let _ = self.entry.delete_credential();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The verifier survives a round trip through the challenge (the challenge
    /// is deterministic and non-empty, and never leaks the verifier).
    #[test]
    fn challenge_is_base64url_sha256() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let expected = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert_eq!(code_challenge(verifier), expected);
    }

    /// A fresh verifier is 32 random bytes, so its challenge changes each call.
    #[test]
    fn verifier_is_random() {
        assert_ne!(generate_verifier(), generate_verifier());
        assert_ne!(generate_state(), generate_state());
    }

    /// The authorize URL carries every PKCE parameter.
    #[test]
    fn authorize_url_carries_pkce() {
        let config = OAuthConfig::tidal(
            "cid".into(),
            "secret".into(),
            "http://127.0.0.1:8080/callback".into(),
            "collection.read".into(),
        );
        let (url, verifier, state) = begin(&config);
        let parsed = reqwest::Url::parse(&url).unwrap();
        let q: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["client_id"], "cid");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["code_challenge"], code_challenge(&verifier));
        assert_eq!(q["state"], state);
    }

    /// The token parser carries the refresh token over on a refresh response.
    #[test]
    fn parse_token_carries_refresh_over() {
        let full = r#"{"access_token":"a","refresh_token":"r","expires_in":86400}"#;
        let t = parse_token(full, "old").unwrap();
        assert_eq!(t.refresh_token, "r");
        assert_eq!(t.expires_in, 86400);

        let refreshed = r#"{"access_token":"b","expires_in":86400}"#;
        let t = parse_token(refreshed, "r").unwrap();
        assert_eq!(t.refresh_token, "r");
        assert_eq!(t.access_token, "b");
    }

    /// The auth state exposes its name and token only while signed in.
    #[test]
    fn auth_name_only_when_signed_in() {
        let a = Auth::default();
        assert_eq!(a.name(), None);
        let a = Auth::SignedIn { token: Token { access_token: "a".into(), refresh_token: "r".into(), expires_in: 1 }, name: "Ada".into() };
        assert_eq!(a.name(), Some("Ada"));
    }

    /// The memory store round-trips a token.
    #[test]
    fn memory_store_round_trips() {
        let store = MemoryStore::default();
        assert_eq!(store.load(), None);
        let token = Token { access_token: "a".into(), refresh_token: "r".into(), expires_in: 1 };
        store.save(&token);
        assert_eq!(store.load(), Some(token));
        store.clear();
        assert_eq!(store.load(), None);
    }

    /// The JWT decoder reads `sub` and the name from the token payload.
    #[test]
    fn decode_claims_reads_sub_and_name() {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let payload = URL_SAFE_NO_PAD.encode(br#"{"sub":"123","username":"ada"}"#);
        let token = format!("h.{payload}.sig");
        let claims = decode_claims(&token).unwrap();
        assert_eq!(claims.sub.as_deref(), Some("123"));
        assert_eq!(claims.display_name().as_deref(), Some("ada"));
    }
}
