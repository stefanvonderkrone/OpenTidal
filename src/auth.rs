//! The OAuth 2.1 authorization-code flow with PKCE (S256), the auth state
//! machine, and the token store. Rune-free: nothing here depends on the view.
//! The HTTP exchange lives in [`crate::tidal`]; this module is the pure state,
//! the crypto, and the parsing, so a test proves it without a network.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::Mutex,
};

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
    /// caller. An empty scope falls back to [`DEFAULT_SCOPE`].
    pub fn tidal(client_id: String, client_secret: String, redirect_uri: String, scope: String) -> Self {
        let scope = if scope.trim().is_empty() { DEFAULT_SCOPE.to_string() } else { scope };
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

/// Every read scope Tidal offers to a third-party client. OpenTidal is
/// read-only, so no write or playback scope is requested. `r_usr` is omitted:
/// Tidal rejects it for third-party clients.
pub const DEFAULT_SCOPE: &str =
    "user.read collection.read playlists.read entitlements.read recommendations.read search.read";

/// The auth state. A variant carries only the data valid in it: a login in
/// flight holds its verifier and state, a signed-in state holds a token.
///
/// The display name is not here. It belongs to the library, which is what a
/// restart reads before any network call, so one stored name serves the
/// sidebar, the Account page, and the profile fetch.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum Auth {
    #[default]
    SignedOut,
    Authorizing { state: String, verifier: String },
    SignedIn { token: Token },
    Failed { message: String },
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

/// The claims of a Tidal access token, which is a JWT. The `uid` claim is the
/// user id (Tidal's tokens carry `uid`, not the OIDC `sub`). `exp` is the
/// absolute expiry, in seconds since the Unix epoch.
///
/// The token carries no display name, so the profile name comes from
/// `/users/me` in [`crate::tidal`].
#[derive(Default, Deserialize)]
pub struct JwtClaims {
    #[serde(default)]
    pub uid: Option<serde_json::Value>,
    #[serde(default)]
    pub sub: Option<String>,
    #[serde(default)]
    pub exp: Option<u64>,
}

impl JwtClaims {
    /// The user id: the `uid` claim Tidal puts in its tokens, with `sub` as a
    /// fallback for any token that uses the OIDC name. `uid` is a number or a
    /// string depending on the token, so both are accepted.
    pub fn user_id(&self) -> Option<String> {
        let uid = self.uid.as_ref().and_then(|v| match v {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        });
        uid.or_else(|| self.sub.clone()).filter(|s| !s.is_empty())
    }
}

/// Decode the payload of a JWT access token. Returns `None` when the token is
/// not a JWT or its payload is not the expected shape.
pub fn decode_claims(token: &str) -> Option<JwtClaims> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Refresh this far before the `exp` time, so a request that races the expiry
/// still uses a token the API accepts.
const EXPIRY_SKEW: u64 = 60;

/// Whether a token's access token has passed its JWT `exp` time (within
/// [`EXPIRY_SKEW`]). A token that is not a JWT, or one with no `exp` claim, is
/// treated as valid, so the fake and any unknown shape do not trip this check.
pub fn token_expired(token: &Token) -> bool {
    let Some(claims) = decode_claims(&token.access_token) else {
        return false;
    };
    let Some(exp) = claims.exp else {
        return false;
    };
    now_secs().saturating_add(EXPIRY_SKEW) >= exp
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Where the refresh token persists between runs.
pub trait TokenStore: Send + Sync {
    fn load(&self) -> Option<Token>;
    fn save(&self, token: &Token);
    /// Clear the stored token. Fails when the underlying write is rejected, so
    /// the caller can tell that a persistent session was not cleared.
    fn clear(&self) -> Result<(), String>;
    /// Whether this store keeps the token in the process and nowhere else. A test
    /// that must not touch the real keychain asks this instead of probing with a
    /// write, because a probe into the real store is the very harm it guards against.
    ///
    /// Asked on the trait rather than by type, because the scene and the test both
    /// hold an `Arc<dyn TokenStore>`. The default is the careful answer: an unknown
    /// store is treated as one that can persist.
    fn is_memory(&self) -> bool;
}

/// An in-memory store for tests and non-macOS platforms.
#[derive(Default)]
pub struct MemoryStore {
    token: Mutex<Option<Token>>,
}

impl TokenStore for MemoryStore {
    fn is_memory(&self) -> bool {
        true
    }

    fn load(&self) -> Option<Token> {
        self.token.lock().unwrap().clone()
    }
    fn save(&self, token: &Token) {
        *self.token.lock().unwrap() = Some(token.clone());
    }
    fn clear(&self) -> Result<(), String> {
        *self.token.lock().unwrap() = None;
        Ok(())
    }
}

/// The path of the token file. An absolute XDG_DATA_HOME wins; otherwise the
/// path falls back to `~/.local/share`. Pure, so a test needs no environment.
pub fn token_file_path(xdg_data_home: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    let base = xdg_data_home
        .map(Path::new)
        .filter(|path| !path.as_os_str().is_empty() && path.is_absolute())
        .map(Path::to_path_buf)
        .or_else(|| home.filter(|path| !path.as_os_str().is_empty()).map(|path| path.join(".local/share")))?;
    Some(base.join("opentidal/auth.toml"))
}

/// A token store that keeps the token in one TOML file.
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    /// Make a store for one token file.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Make a store at the path the environment selects, if there is one.
    pub fn from_env() -> Option<Self> {
        let xdg_data_home = std::env::var_os("XDG_DATA_HOME");
        let home = std::env::var_os("HOME");
        token_file_path(xdg_data_home.as_deref(), home.as_deref().map(Path::new)).map(Self::new)
    }

    fn write(&self, token: &Token) -> std::io::Result<()> {
        let text = toml::to_string(token).map_err(std::io::Error::other)?;
        crate::atomic::replace(&self.path, text.as_bytes())?;
        exclude_from_backup(&self.path);
        Ok(())
    }
}

/// Keep the token out of a Time Machine backup, using the same extended
/// attribute that `Store::open` sets on the database.
#[cfg(target_os = "macos")]
fn exclude_from_backup(path: &Path) {
    let _ = std::process::Command::new("xattr")
        .args([
            "-w",
            "com.apple.metadata:com_apple_backup_excludeItem",
            "com.apple.backupd",
            &path.to_string_lossy(),
        ])
        .status();
}

/// No Time Machine off macOS. The call site in `write` is unconditional.
#[cfg(not(target_os = "macos"))]
fn exclude_from_backup(_path: &Path) {}

impl TokenStore for FileStore {
    fn is_memory(&self) -> bool {
        false
    }

    fn load(&self) -> Option<Token> {
        toml::from_str::<Token>(&std::fs::read_to_string(&self.path).ok()?).ok()
    }

    fn save(&self, token: &Token) {
        if let Err(error) = self.write(token) {
            eprintln!("opentidal: cannot write tokens to {}: {error}", self.path.display());
        }
    }

    fn clear(&self) -> Result<(), String> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
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
    fn is_memory(&self) -> bool {
        false
    }
    fn load(&self) -> Option<Token> {
        let json = self.entry.get_password().ok()?;
        serde_json::from_str(&json).ok()
    }
    fn save(&self, token: &Token) {
        if let Ok(json) = serde_json::to_string(token) {
            let _ = self.entry.set_password(&json);
        }
    }
    fn clear(&self) -> Result<(), String> {
        // Overwrite, not delete, so the item keeps its access-control list. A
        // delete makes the next sign-in create a fresh item, and macOS prompts
        // again for an unsigned binary. The sentinel is not JSON, so `load`
        // reads it as no token. A rejected write is surfaced, so the caller
        // does not claim sign-out when the token still persists.
        self.entry.set_password("signed-out").map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_file_lives_under_xdg_data_home() {
        assert_eq!(
            token_file_path(Some(OsStr::new("/data")), Some(Path::new("/home/user"))),
            Some(PathBuf::from("/data/opentidal/auth.toml"))
        );
        assert_eq!(
            token_file_path(Some(OsStr::new("/data")), None),
            Some(PathBuf::from("/data/opentidal/auth.toml"))
        );
    }

    #[test]
    fn without_xdg_data_home_the_token_file_falls_back_to_local_share() {
        for xdg in [None, Some(OsStr::new("")), Some(OsStr::new("relative"))] {
            assert_eq!(
                token_file_path(xdg, Some(Path::new("/home/user"))),
                Some(PathBuf::from("/home/user/.local/share/opentidal/auth.toml"))
            );
            assert_eq!(token_file_path(xdg, None), None);
            assert_eq!(token_file_path(xdg, Some(Path::new(""))), None);
        }
    }

    #[test]
    fn a_file_store_survives_a_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("opentidal/auth.toml");
        let token = Token { access_token: "a".into(), refresh_token: "r".into(), expires_in: 86400 };
        let store = FileStore::new(path.clone());
        assert!(!store.is_memory());
        assert_eq!(store.load(), None);
        store.save(&token);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "access_token = \"a\"\nrefresh_token = \"r\"\nexpires_in = 86400\n"
        );
        let restarted = FileStore::new(path);
        assert_eq!(restarted.load(), Some(token));
        restarted.clear().unwrap();
        assert_eq!(restarted.load(), None);
        assert!(restarted.clear().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn the_token_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("opentidal/auth.toml");
        let store = FileStore::new(path.clone());
        store.save(&Token { access_token: "a".into(), refresh_token: "r".into(), expires_in: 1 });
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        #[cfg(target_os = "macos")]
        {
            let out = std::process::Command::new("xattr")
                .args(["-p", "com.apple.metadata:com_apple_backup_excludeItem", &path.to_string_lossy()])
                .output()
                .unwrap();
            assert!(String::from_utf8_lossy(&out.stdout).contains("com.apple.backupd"));
        }
    }

    #[test]
    fn a_failed_write_keeps_the_previous_token() {
        if std::process::Command::new("id").arg("-u").output().is_ok_and(|output| output.stdout == b"0\n") {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("opentidal/auth.toml");
        let store = FileStore::new(path.clone());
        let token = Token { access_token: "a".into(), refresh_token: "r".into(), expires_in: 1 };
        store.save(&token);
        let temporary = path.with_extension(format!("toml.{}.tmp", std::process::id()));
        std::fs::create_dir(&temporary).unwrap();
        store.save(&Token { access_token: "b".into(), ..token.clone() });
        assert_eq!(store.load(), Some(token));
    }

    #[test]
    fn a_failed_write_leaves_no_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("opentidal/auth.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // A directory at the target makes the rename fail after the temporary
        // file has been written, which is where a half-written file would stay.
        std::fs::create_dir(&path).unwrap();
        FileStore::new(path).save(&Token { access_token: "a".into(), refresh_token: "r".into(), expires_in: 1 });
        let leftovers: Vec<_> = std::fs::read_dir(directory.path().join("opentidal"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "a failed write left a temporary token file");
    }

    #[test]
    fn a_corrupt_token_file_reads_as_signed_out() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.toml");
        std::fs::write(&path, "not a token").unwrap();
        assert_eq!(FileStore::new(path).load(), None);
    }

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

    /// The memory store round-trips a token.
    #[test]
    fn memory_store_round_trips() {
        let store = MemoryStore::default();
        assert_eq!(store.load(), None);
        let token = Token { access_token: "a".into(), refresh_token: "r".into(), expires_in: 1 };
        store.save(&token);
        assert_eq!(store.load(), Some(token));
        store.clear().unwrap();
        assert_eq!(store.load(), None);
    }

    /// The JWT decoder reads `sub` and `uid` from the token payload.
    #[test]
    fn decode_claims_reads_sub_and_uid() {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let payload = URL_SAFE_NO_PAD.encode(br#"{"sub":"123","uid":"456"}"#);
        let token = format!("h.{payload}.sig");
        let claims = decode_claims(&token).unwrap();
        assert_eq!(claims.sub.as_deref(), Some("123"));
        assert_eq!(claims.user_id().as_deref(), Some("456"));
    }

    /// Tidal tokens carry the user id in `uid`; `user_id` prefers it over `sub`
    /// and accepts `uid` as a number or a string.
    #[test]
    fn user_id_prefers_uid_over_sub() {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let payload = URL_SAFE_NO_PAD.encode(br#"{"uid":42,"sub":"43"}"#);
        let token = format!("h.{payload}.sig");
        let claims = decode_claims(&token).unwrap();
        assert_eq!(claims.user_id().as_deref(), Some("42"));

        let payload = URL_SAFE_NO_PAD.encode(br#"{"uid":"42"}"#);
        let token = format!("h.{payload}.sig");
        assert_eq!(decode_claims(&token).unwrap().user_id().as_deref(), Some("42"));

        let payload = URL_SAFE_NO_PAD.encode(br#"{"sub":"43"}"#);
        let token = format!("h.{payload}.sig");
        assert_eq!(decode_claims(&token).unwrap().user_id().as_deref(), Some("43"));
    }

    /// Expiry comes from the JWT `exp` claim; a token without one is valid.
    #[test]
    fn expiry_comes_from_the_exp_claim() {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let past = now_secs() - 10;
        let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"sub":"1","exp":{past}}}"#));
        let expired = Token { access_token: format!("h.{payload}.sig"), refresh_token: "r".into(), expires_in: 1 };
        assert!(token_expired(&expired));

        let future = now_secs() + 600;
        let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"sub":"1","exp":{future}}}"#));
        let valid = Token { access_token: format!("h.{payload}.sig"), refresh_token: "r".into(), expires_in: 1 };
        assert!(!token_expired(&valid));

        let no_exp = Token { access_token: "h.".to_string(), refresh_token: "r".into(), expires_in: 1 };
        assert!(!token_expired(&no_exp));
    }
}
