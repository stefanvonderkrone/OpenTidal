//! The Tidal boundary: the remote data types, the [`MusicService`] port, and
//! its two impls. [`FakeService`] is deterministic and backs every proof and
//! headless drive. [`HttpService`] is a thin blocking `reqwest` client over the
//! Tidal JSON:API.
//!
//! This module is rune-free. It holds no network or token state in `State`; the
//! app wraps a service in `Arc` on [`crate::app::App`].

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;

use crate::auth::{OAuthConfig, Token};
use crate::catalogue::{AlbumRow, ArtistRow, ArtworkId, CatalogueRows, CoverOwner, CoverRequest, PlaylistRow, TrackRow};
use crate::library::RemoteList;

/// A Tidal user id, an opaque string. The newtype keeps it from being mistaken
/// for any other string. Catalogue ids have their own types in `catalogue.rs`.
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
    /// The refresh token was revoked; the stored token must be cleared.
    InvalidGrant(String),
    /// The token was rejected as expired or invalid; refresh and retry.
    Unauthorized,
    /// The transport or the API returned a non-success status.
    Http(String),
    /// The response body did not match the expected shape.
    Decode(String),
    /// A paged read hit the page cap or a cursor it had already followed.
    Paging(String),
    /// The service kept answering `429` after every retry the pacer allows.
    RateLimited(String),
    /// The artist has no profile art, or Tidal does not know the artist. Not a
    /// failure, so it is never logged.
    NoPicture,
}

/// Where the next page of a remote list starts. The fields are private, so only
/// this module makes one. `path` is under the service base, already checked by
/// `under_base`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Cursor {
    leg: Leg,
    path: String,
}

#[cfg(test)]
impl Cursor {
    /// The cursor `FakeService` mints for page `path`, so a test in another
    /// module can build a page by hand.
    pub fn fake(path: &str) -> Cursor {
        Cursor { leg: Leg::Last, path: path.to_string() }
    }
}

/// Which read of a list a cursor is on. Only `RemoteList::Playlists` has two:
/// the favorited playlists, then the owned ones.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
enum Leg {
    Favorited,
    Last,
}

/// One page of one remote list, decoded from one response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub rows: CatalogueRows,
    /// The member ids of this page, in Tidal order.
    pub members: Vec<String>,
    /// `None` on the last page. The last favorited page of
    /// `RemoteList::Playlists` points at the first owned page, so the two reads
    /// make one chain.
    pub next: Option<Cursor>,
}

/// The read-only music port. Every method takes the access token, so the fake
/// ignores it and the real client sends it as a bearer header.
pub trait MusicService: Send + Sync {
    fn current_user(&self, token: &str) -> Result<User, ServiceError>;
    /// One page of `of`. `after: None` reads the first page. The caller owns
    /// the page cap and the repeated-cursor check.
    fn page(&self, token: &str, user: &str, of: &RemoteList, after: Option<&Cursor>) -> Result<Page, ServiceError>;
    /// The bytes of one cover, near [`COVER_PX`] wide. Unchecked: the cover
    /// cache checks them before a put.
    fn cover(&self, token: &str, cover: &CoverRequest) -> Result<Vec<u8>, ServiceError>;
    /// The pictures of many artists, one result per request, in request order.
    /// An artist with no picture is [`ServiceError::NoPicture`].
    fn artist_pictures(&self, token: &str, requests: &[CoverRequest]) -> Vec<(ArtworkId, Result<Vec<u8>, ServiceError>)> {
        requests.iter().map(|request| (request.artwork.clone(), self.cover(token, request))).collect()
    }
}

/// The most artist ids one `filter[id]` read takes. 20 answered live, and 50
/// answered `400`.
const ARTIST_BATCH: usize = 20;

/// The width a cover fetch asks for: 168 pt cards and 200 pt heroes at 2x.
pub const COVER_PX: u64 = 640;

/// The largest image body a cover fetch accepts.
pub const MAX_COVER_BYTES: u64 = 4 * 1024 * 1024;

/// A fixed, deterministic service for tests and headless drives.
pub struct FakeService {
    pub user: User,
    /// What `page` answers, per list, in order. The service sets each `next`
    /// itself. A list with no entry answers one empty last page.
    pub pages: BTreeMap<RemoteList, Vec<Result<Page, ServiceError>>>,
    /// What `cover` answers, by artwork id. A missing id is `Http("404")`.
    pub covers: BTreeMap<String, Result<Vec<u8>, ServiceError>>,
    /// How many times `cover` ran, so a test proves a hit made no fetch.
    pub cover_calls: AtomicUsize,
    /// How many batched artist reads `HttpService` would make for the
    /// `artist_pictures` calls so far: one per [`ARTIST_BATCH`] requests.
    pub artist_picture_calls: AtomicUsize,
}

impl Default for FakeService {
    fn default() -> Self {
        FakeService {
            user: User { id: UserId("fake-user".to_string()), name: "Fake User".to_string() },
            pages: BTreeMap::new(),
            covers: BTreeMap::new(),
            cover_calls: AtomicUsize::new(0),
            artist_picture_calls: AtomicUsize::new(0),
        }
    }
}

impl MusicService for FakeService {
    fn current_user(&self, _token: &str) -> Result<User, ServiceError> {
        Ok(self.user.clone())
    }
    /// Page `n` answers the cursor whose path is `n`.
    fn page(&self, _token: &str, _user: &str, of: &RemoteList, after: Option<&Cursor>) -> Result<Page, ServiceError> {
        let empty = || Ok(Page { rows: CatalogueRows::default(), members: Vec::new(), next: None });
        let pages = self.pages.get(of).cloned().unwrap_or_else(|| vec![empty()]);
        let index = match after {
            None => 0,
            Some(cursor) => cursor
                .path
                .parse()
                .map_err(|_| ServiceError::Paging(format!("not a fake cursor: {}", cursor.path)))?,
        };
        let page = pages
            .get(index)
            .cloned()
            .unwrap_or_else(|| Err(ServiceError::Paging(format!("no fake page {index}"))))?;
        let next = (index + 1 < pages.len()).then(|| Cursor { leg: Leg::Last, path: (index + 1).to_string() });
        Ok(Page { next, ..page })
    }
    fn cover(&self, _token: &str, cover: &CoverRequest) -> Result<Vec<u8>, ServiceError> {
        self.cover_calls.fetch_add(1, Ordering::SeqCst);
        self.covers
            .get(cover.artwork.as_str())
            .cloned()
            .unwrap_or_else(|| Err(ServiceError::Http("404".to_string())))
    }
    /// Answers from the cover map. An artist with no entry has no picture.
    fn artist_pictures(&self, _token: &str, requests: &[CoverRequest]) -> Vec<(ArtworkId, Result<Vec<u8>, ServiceError>)> {
        self.artist_picture_calls.fetch_add(requests.chunks(ARTIST_BATCH).count(), Ordering::SeqCst);
        requests
            .iter()
            .map(|request| {
                let answer = self.covers.get(request.artwork.as_str()).cloned().unwrap_or(Err(ServiceError::NoPicture));
                (request.artwork.clone(), answer)
            })
            .collect()
    }
}

/// The time source the pacer waits on. Tests pass a fake, so no test sleeps.
trait Clock: Send + Sync {
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
}

struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// The least time between two requests. Tidal answered `429` on the tenth
/// unpaced page request of a live sync.
const MIN_GAP: Duration = Duration::from_millis(300);
/// The wait after the first `429` that names no `Retry-After`. It doubles on
/// each retry.
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
const MAX_RETRIES: u32 = 4;
/// The most one request waits on `429` answers before it gives up.
const MAX_WAIT: Duration = Duration::from_secs(60);

/// Spaces every JSON:API request of one service by [`MIN_GAP`] and retries a
/// `429`. It paces only `openapi.tidal.com`. Image downloads go to
/// `resources.tidal.com`, which answered 30 rapid requests with no `429`. The
/// service and the cover worker share it, so a `429` on one thread holds back
/// the other as well.
struct Pacer {
    clock: Arc<dyn Clock>,
    /// The earliest time the next request may start.
    next: Mutex<Option<Instant>>,
}

impl Pacer {
    /// Wait for the next free slot, then take it.
    fn wait_turn(&self) {
        let mut next = self.next.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(at) = *next {
            let now = self.clock.now();
            if at > now {
                self.clock.sleep(at - now);
            }
        }
        *next = Some(self.clock.now() + MIN_GAP);
    }

    /// Hold every request back until `delay` has passed.
    fn back_off(&self, delay: Duration) {
        let mut next = self.next.lock().unwrap_or_else(PoisonError::into_inner);
        let until = self.clock.now() + delay;
        *next = Some(next.map_or(until, |at| at.max(until)));
    }

    /// Send through `attempt`, retrying a `429` after its `Retry-After`, or
    /// after a doubling backoff when it names none. `what` names the request in
    /// the error.
    fn send(
        &self,
        what: &str,
        mut attempt: impl FnMut() -> Result<reqwest::blocking::Response, ServiceError>,
    ) -> Result<reqwest::blocking::Response, ServiceError> {
        let mut retries = 0;
        let mut waited = Duration::ZERO;
        let mut backoff = FIRST_BACKOFF;
        loop {
            self.wait_turn();
            let resp = attempt()?;
            if resp.status() != reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Ok(resp);
            }
            let delay = retry_after(resp.headers()).unwrap_or(backoff);
            if retries == MAX_RETRIES || waited + delay > MAX_WAIT {
                return Err(ServiceError::RateLimited(format!("429 Too Many Requests for {what} after {retries} retries")));
            }
            retries += 1;
            waited += delay;
            backoff *= 2;
            self.back_off(delay);
        }
    }
}

/// A `Retry-After` in seconds. The HTTP-date form is read as absent, so the
/// backoff applies.
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    value.trim().parse().ok().map(Duration::from_secs)
}

/// The real client over the Tidal JSON:API (`https://openapi.tidal.com/v2`).
pub struct HttpService {
    client: reqwest::blocking::Client,
    base: String,
    /// The path of `base`, such as `/v2`. A `next` link starts with it.
    prefix: String,
    pacer: Pacer,
}

impl HttpService {
    pub fn new(base: &str) -> Self {
        Self::with_clock(base, Arc::new(SystemClock))
    }

    fn with_clock(base: &str, clock: Arc<dyn Clock>) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("http client");
        let base = base.trim_end_matches('/').to_string();
        let prefix = reqwest::Url::parse(&base)
            .map(|url| url.path().trim_end_matches('/').to_string())
            .unwrap_or_default();
        HttpService { client, base, prefix, pacer: Pacer { clock, next: Mutex::new(None) } }
    }

    fn get_doc(&self, token: &str, path: &str) -> Result<Document, ServiceError> {
        let url = format!("{}{}", self.base, path);
        let resp = self.pacer.send(path, || {
            self.client
                .get(&url)
                .bearer_auth(token)
                .header("accept", "application/vnd.api+json")
                .send()
                .map_err(|e| ServiceError::Http(e.to_string()))
        })?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ServiceError::Unauthorized);
        }
        if !status.is_success() {
            return Err(ServiceError::Http(format!("{status} for {path}")));
        }
        resp.json::<Document>().map_err(|e| ServiceError::Decode(e.to_string()))
    }

    /// GET an image with no `Authorization` header, a 15 s timeout, and a body
    /// cap of [`MAX_COVER_BYTES`]. It takes no token, so the token cannot reach
    /// the image host. The pacer does not hold it back, because the image host
    /// is not the JSON:API host the pacer protects.
    fn get_image(&self, url: &reqwest::Url) -> Result<Vec<u8>, ServiceError> {
        use std::io::Read;
        let resp = self
            .client
            .get(url.clone())
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .map_err(|e| ServiceError::Http(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(ServiceError::Http(format!("{status} for a cover image")));
        }
        let mut body = Vec::new();
        resp.take(MAX_COVER_BYTES + 1)
            .read_to_end(&mut body)
            .map_err(|e| ServiceError::Http(e.to_string()))?;
        if body.len() as u64 > MAX_COVER_BYTES {
            return Err(ServiceError::Http("a cover image is over the size cap".to_string()));
        }
        Ok(body)
    }
}

impl MusicService for HttpService {
    /// The user id comes from the token's `uid` claim. The name always comes
    /// from `/users/me` (the third-party client cannot read `/users/{id}`, and
    /// Tidal tokens carry no name claim), so an expired or revoked token is
    /// rejected even when its JWT happened to hold a name.
    fn current_user(&self, token: &str) -> Result<User, ServiceError> {
        let claims = crate::auth::decode_claims(token)
            .ok_or_else(|| ServiceError::Decode("token is not a JWT".to_string()))?;
        let id = claims.user_id().ok_or_else(|| ServiceError::Decode("token has no uid claim".to_string()))?;
        let name = match self.get_doc(token, "/users/me") {
            Ok(Document { data: Some(Data::One(profile)), .. }) => name_from(&profile.attributes),
            Ok(_) => "You".to_string(),
            // Surface an expired token so the caller refreshes and retries;
            // any other failure keeps the best-effort fallback name.
            Err(ServiceError::Unauthorized) => return Err(ServiceError::Unauthorized),
            Err(_) => "You".to_string(),
        };
        Ok(User { id: UserId(id), name })
    }

    fn page(&self, token: &str, user: &str, of: &RemoteList, after: Option<&Cursor>) -> Result<Page, ServiceError> {
        fetch_page(&mut |path: &str| self.get_doc(token, path), &self.prefix, user, of, after)
    }

    fn cover(&self, token: &str, cover: &CoverRequest) -> Result<Vec<u8>, ServiceError> {
        fetch_cover(&mut |path: &str| self.get_doc(token, path), &mut |url| self.get_image(url), cover)
    }

    fn artist_pictures(&self, token: &str, requests: &[CoverRequest]) -> Vec<(ArtworkId, Result<Vec<u8>, ServiceError>)> {
        fetch_artist_pictures(&mut |path: &str| self.get_doc(token, path), &mut |url| self.get_image(url), requests)
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
        return Err(token_error(status, &body));
    }
    crate::auth::parse_token(&body, fallback_refresh).map_err(ServiceError::Decode)
}

/// The `error` field of a token-endpoint error body, when the body is JSON with
/// one. Any other shape (or no body at all) reads as no OAuth error.
fn oauth_error(body: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct OAuthError {
        #[serde(default)]
        error: Option<String>,
    }
    serde_json::from_str::<OAuthError>(body).ok().and_then(|e| e.error)
}

/// Classify a non-success token-endpoint status. Only a `400` or `401` whose
/// body names `invalid_grant` is permanent; every other failure is transient
/// and maps to [`ServiceError::Auth`] so the stored token is kept.
fn token_error(status: reqwest::StatusCode, body: &str) -> ServiceError {
    let invalid_grant = (status == reqwest::StatusCode::BAD_REQUEST
        || status == reqwest::StatusCode::UNAUTHORIZED)
        && oauth_error(body).as_deref() == Some("invalid_grant");
    if invalid_grant {
        ServiceError::InvalidGrant(format!("{status}: {body}"))
    } else {
        ServiceError::Auth(format!("{status}: {body}"))
    }
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

/// How long one accepted callback connection may stay idle on read or write
/// before the exchange gives up, so a connection that opens but sends nothing
/// cannot block the worker forever.
const CALLBACK_IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

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
                let _ = stream.set_read_timeout(Some(CALLBACK_IO_TIMEOUT));
                let _ = stream.set_write_timeout(Some(CALLBACK_IO_TIMEOUT));
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

/// The JSON:API document envelope. `data` is one resource for a profile and an
/// array for a list. `included` holds the resources a list names, and
/// `links.next` the next page. Members the app does not read are ignored.
#[derive(Deserialize)]
struct Document {
    #[serde(default)]
    data: Option<Data>,
    #[serde(default)]
    included: Vec<Resource>,
    #[serde(default)]
    links: Links,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Data {
    One(Resource),
    Many(Vec<Resource>),
}

#[derive(Deserialize, Default)]
struct Links {
    next: Option<String>,
}

#[derive(Deserialize)]
struct Resource {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    id: String,
    attributes: Option<Value>,
    #[serde(default)]
    relationships: Value,
}

impl Resource {
    fn text(&self, key: &str) -> Option<String> {
        self.attributes.as_ref()?.get(key)?.as_str().map(str::to_owned)
    }

    /// The id of the first resource a relationship names, such as the album of
    /// a track. Empty when absent, which `absorb` drops and counts.
    fn first(&self, relationship: &str) -> String {
        self.relationships
            .get(relationship)
            .and_then(|r| r.get("data"))
            .and_then(|d| d.get(0))
            .and_then(|r| r.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    }

    /// Add this resource to the catalogue rows, by type. A membership entry
    /// has no attributes and adds nothing.
    fn add_to(&self, rows: &mut CatalogueRows) {
        if self.attributes.is_none() {
            return;
        }
        let id = self.id.clone();
        match self.kind.as_str() {
            "artists" => rows.artists.push(ArtistRow { id, name: self.text("name").unwrap_or_default() }),
            "albums" => rows.albums.push(AlbumRow {
                id,
                title: self.text("title").unwrap_or_default(),
                artist: self.first("artists"),
                cover: self.first("coverArt"),
            }),
            "tracks" => rows.tracks.push(TrackRow {
                id,
                title: self.text("title").unwrap_or_default(),
                artist: self.first("artists"),
                album: self.first("albums"),
                duration: self.text("duration").unwrap_or_default(),
            }),
            "playlists" => rows.playlists.push(PlaylistRow {
                id,
                name: self.text("name").unwrap_or_default(),
                description: self.text("description").unwrap_or_default(),
                tracks: Vec::new(),
                cover: self.first("coverArt"),
            }),
            _ => {}
        }
    }
}

impl Document {
    fn resources(&self) -> impl Iterator<Item = &Resource> {
        let data: &[Resource] = match &self.data {
            Some(Data::One(r)) => std::slice::from_ref(r),
            Some(Data::Many(rs)) => rs,
            None => &[],
        };
        data.iter()
    }

    /// The ids in `data` of one type, in order: the members a list page holds.
    fn members(&self, kind: &str) -> impl Iterator<Item = String> {
        self.resources().filter(move |r| r.kind == kind).map(|r| r.id.clone())
    }

    /// Every resource with attributes, in `data` and `included`, as catalogue rows.
    fn add_to(&self, rows: &mut CatalogueRows) {
        for r in self.resources().chain(&self.included) {
            r.add_to(rows);
        }
    }
}

/// One page source: a path under the service base in, a document out.
type PageSource<'a> = dyn FnMut(&str) -> Result<Document, ServiceError> + 'a;

/// A `next` link as a path under the service base. Tidal sends an absolute path
/// from the host root in one of two shapes: with the base path, such as
/// `/v2/...` for the base `https://openapi.tidal.com/v2`, or without it, such
/// as `/userCollectionAlbums/...`, which the live service sends. The base path
/// is stripped only when it is there. Anything that is not an absolute path,
/// such as a full URL or a `//host` link, is refused. `get_doc` joins every
/// path to the base, so the bearer token never leaves the service.
fn under_base(prefix: &str, next: &str) -> Result<String, ServiceError> {
    let path = match next.strip_prefix(prefix) {
        Some(rest) if rest.starts_with('/') => rest,
        _ => next,
    };
    if path.starts_with('/') && !path.starts_with("//") {
        Ok(path.to_owned())
    } else {
        Err(ServiceError::Decode(format!("a next link leaves the service: {next}")))
    }
}

const LIKED: &str =
    "/userCollectionTracks/me/relationships/items?include=items,items.albums,items.artists,items.albums.coverArt,items.albums.artists";
const SAVED: &str = "/userCollectionAlbums/me/relationships/items?include=items,items.artists,items.coverArt";
const FOLLOWED: &str = "/userCollectionArtists/me/relationships/items?include=items";
const FAVORITED: &str = "/userCollectionPlaylists/me/relationships/items?include=items,items.coverArt";

/// The account's playlists, by owner. The brackets are percent-encoded, the
/// way Tidal writes them in its own links.
fn owned(user: &str) -> String {
    format!("/playlists?filter%5Bowners.id%5D={user}")
}

fn playlist_items(id: &str) -> String {
    format!(
        "/playlists/{id}/relationships/items?include=items,items.albums,items.artists,items.albums.coverArt,items.albums.artists"
    )
}

/// The tracks of one album. The included albums name their artists, so each
/// track's album resolves.
fn album_items(id: &str) -> String {
    format!(
        "/albums/{id}/relationships/items?include=items,items.artists,items.albums,items.albums.coverArt,items.albums.artists"
    )
}

/// The albums of one artist. `/artists/{id}/relationships/tracks` answered
/// `400` live with every include tried, so an artist reads its albums only.
fn artist_albums(id: &str) -> String {
    format!("/artists/{id}/relationships/albums?include=albums,albums.artists,albums.coverArt")
}

/// One page of `of`, over any page source. `after: None` starts at the first
/// favorited page for `RemoteList::Playlists`, so its owned read follows.
fn fetch_page(
    get: &mut PageSource,
    prefix: &str,
    user: &str,
    of: &RemoteList,
    after: Option<&Cursor>,
) -> Result<Page, ServiceError> {
    let (first, kind, leg) = match of {
        RemoteList::Liked => (LIKED.to_owned(), "tracks", Leg::Last),
        RemoteList::Albums => (SAVED.to_owned(), "albums", Leg::Last),
        RemoteList::Artists => (FOLLOWED.to_owned(), "artists", Leg::Last),
        RemoteList::Playlists => (FAVORITED.to_owned(), "playlists", Leg::Favorited),
        RemoteList::Playlist(id) => (playlist_items(id.as_str()), "tracks", Leg::Last),
        RemoteList::Album(id) => (album_items(id.as_str()), "tracks", Leg::Last),
        RemoteList::Artist(id) => (artist_albums(id.as_str()), "albums", Leg::Last),
    };
    let at = after.cloned().unwrap_or(Cursor { leg, path: first });
    let doc = get(&at.path)?;
    let mut rows = CatalogueRows::default();
    doc.add_to(&mut rows);
    let members = doc.members(kind).collect();
    let next = match (doc.links.next, at.leg) {
        (Some(next), leg) => Some(Cursor { leg, path: under_base(prefix, &next)? }),
        (None, Leg::Favorited) => Some(Cursor { leg: Leg::Last, path: owned(user) }),
        (None, Leg::Last) => None,
    };
    Ok(Page { rows, members, next })
}

/// The `coverArt` read of an album or a playlist, with the artwork included.
fn cover_path(owner: &CoverOwner) -> Option<String> {
    match owner {
        CoverOwner::Album(id) => Some(format!("/albums/{}/relationships/coverArt?include=coverArt", id.as_str())),
        CoverOwner::Playlist(id) => Some(format!("/playlists/{}/relationships/coverArt?include=coverArt", id.as_str())),
        CoverOwner::Artist(_) => None,
    }
}

/// The batched read of up to [`ARTIST_BATCH`] artists with their profile art.
/// The brackets and the commas are percent-encoded.
fn artists_path(ids: &[&str]) -> String {
    format!("/artists?filter%5Bid%5D={}&include=profileArt", ids.join("%2C"))
}

type ImageSource<'a> = dyn FnMut(&reqwest::Url) -> Result<Vec<u8>, ServiceError> + 'a;

/// The whole cover read, over any sources. `get_doc` carries the bearer token
/// to the service. `get_image` takes only a URL, so the token cannot reach the
/// image host by construction. An artist goes through the batched read.
///
/// The included artwork is matched by id only, because its type is
/// `coverArts` in the library fixtures and `artworks` in the spec.
fn fetch_cover(get_doc: &mut PageSource, get_image: &mut ImageSource, cover: &CoverRequest) -> Result<Vec<u8>, ServiceError> {
    let Some(path) = cover_path(&cover.owner) else {
        let mut one = fetch_artist_pictures(get_doc, get_image, std::slice::from_ref(cover));
        return one.pop().map_or(Err(ServiceError::NoPicture), |(_, result)| result);
    };
    let doc = get_doc(&path)?;
    let artwork = doc
        .included
        .iter()
        .find(|r| r.id == cover.artwork.as_str())
        .ok_or_else(|| ServiceError::Decode("the cover response does not include its artwork".to_string()))?;
    get_image(&file_url(artwork)?)
}

/// The pictures of many artists, one `filter[id]` read per [`ARTIST_BATCH`]
/// requests, then one image read per picture. One result per request, in
/// request order. A failed read fails every request of its chunk. An id Tidal
/// does not know is absent from the answer, so it has no picture, the same as
/// an artist with an empty `profileArt`. A request that is not an artist's
/// takes the single cover read.
fn fetch_artist_pictures(
    get_doc: &mut PageSource,
    get_image: &mut ImageSource,
    requests: &[CoverRequest],
) -> Vec<(ArtworkId, Result<Vec<u8>, ServiceError>)> {
    let mut results = Vec::with_capacity(requests.len());
    for chunk in requests.chunks(ARTIST_BATCH) {
        let ids: Vec<&str> = chunk
            .iter()
            .filter_map(|r| match &r.owner {
                CoverOwner::Artist(id) => Some(id.as_str()),
                CoverOwner::Album(_) | CoverOwner::Playlist(_) => None,
            })
            .collect();
        let doc = if ids.is_empty() { None } else { Some(get_doc(&artists_path(&ids))) };
        for request in chunk {
            let result = match (&request.owner, &doc) {
                (CoverOwner::Artist(id), Some(Ok(doc))) => artist_picture(doc, id.as_str(), get_image),
                (CoverOwner::Artist(_), Some(Err(error))) => Err(error.clone()),
                _ => fetch_cover(get_doc, get_image, request),
            };
            results.push((request.artwork.clone(), result));
        }
    }
    results
}

/// One artist's picture from a batched read: the artwork its `profileArt`
/// names, found in `included`.
fn artist_picture(doc: &Document, artist: &str, get_image: &mut ImageSource) -> Result<Vec<u8>, ServiceError> {
    let named = doc
        .resources()
        .find(|r| r.id == artist)
        .map(|r| r.first("profileArt"))
        .filter(|id| !id.is_empty())
        .ok_or(ServiceError::NoPicture)?;
    let artwork = doc.included.iter().find(|r| r.id == named).ok_or(ServiceError::NoPicture)?;
    get_image(&file_url(artwork)?)
}

/// The https URL of the file [`pick_file`] chooses from an artwork.
fn file_url(artwork: &Resource) -> Result<reqwest::Url, ServiceError> {
    let href = pick_file(artwork.attributes.as_ref())
        .ok_or_else(|| ServiceError::Decode("the artwork names no file".to_string()))?;
    reqwest::Url::parse(&href)
        .ok()
        .filter(|url| url.scheme() == "https" && url.host_str().is_some_and(|h| !h.is_empty()))
        .ok_or_else(|| ServiceError::Decode(format!("a cover link is not an https URL: {href}")))
}

/// The href of the smallest file at least [`COVER_PX`] wide, else the widest.
/// A file with no href or no width is skipped.
fn pick_file(attributes: Option<&Value>) -> Option<String> {
    let files = attributes?.get("files")?.as_array()?;
    let sized = files.iter().filter_map(|f| {
        let href = f.get("href")?.as_str()?;
        let width = f.get("meta")?.get("width")?.as_u64()?;
        Some((width, href))
    });
    let (wide, narrow): (Vec<_>, Vec<_>) = sized.partition(|(w, _)| *w >= COVER_PX);
    wide.into_iter()
        .min_by_key(|(w, _)| *w)
        .or_else(|| narrow.into_iter().max_by_key(|(w, _)| *w))
        .map(|(_, href)| href.to_owned())
}

/// The user's display name from the resource attributes. The field names vary
/// across the API versions, so read the known ones in order and fall back.
///
/// The order matters for privacy. Tidal returns the account email in
/// `username`, and this name is stored on disk in the clear and shown in the
/// sidebar, so a real name wins over anything that looks like an address.
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
        if let Some(s) = attrs.get(key).and_then(Value::as_str)
            && !s.is_empty()
            && !is_email(s)
        {
            return s.to_string();
        }
    }
    "You".to_string()
}

/// Whether a name looks like an email address. A stored address is one copy of
/// a secret the user did not choose to publish on their own disk.
fn is_email(s: &str) -> bool {
    let (local, domain) = match s.split_once('@') {
        Some(parts) => parts,
        None => return false,
    };
    !local.is_empty() && domain.contains('.') && !domain.starts_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::Catalogue;
    use crate::library::{Library, Merge, Reach};

    /// A synthetic fixture from `tests/fixtures/tidal`, decoded the way
    /// `get_doc` decodes a response body.
    fn fixture(name: &str) -> Document {
        let path = format!("{}/tests/fixtures/tidal/{name}", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
    }

    const EVENING: &str = "f6b9f0a1-6c8e-4d24-9c71-1d0e2f3a4b5c";
    const MORNING: &str = "0c1d2e3f-4a5b-6c7d-8e9f-a0b1c2d3e4f5";
    const CURRENTS: &str = "251380836";
    const TAME_IMPALA: &str = "1566";

    /// The fixture for each path the read asks for. A path with no fixture
    /// panics, so a read that asks for something unexpected names it.
    fn fixture_path(path: &str) -> &'static str {
        let liked_2 = "/userCollectionTracks/me/relationships/items?include=items,items.albums,items.artists&page%5Bcursor%5D=page-2";
        match path {
            p if p == LIKED => "liked-tracks-page-1.json",
            p if p == liked_2 => "liked-tracks-page-2.json",
            p if p == SAVED => "saved-albums.json",
            p if p == FOLLOWED => "followed-artists.json",
            p if p == FAVORITED => "playlists.json",
            p if p == owned("173210380") => "owned-playlists.json",
            p if p == playlist_items(EVENING) => "playlist-items.json",
            p if p == playlist_items(MORNING) => "owned-playlist-items.json",
            p if p == album_items(CURRENTS) => "album-items.json",
            p if p == artist_albums(TAME_IMPALA) => "artist-albums.json",
            other => panic!("the read asked for a path with no fixture: {other}"),
        }
    }

    /// Every page of `of` from the fixtures, merged into `lib` the way the app
    /// lands a pass: each page partial, the last one complete. Returns how
    /// many ids the merges could not keep.
    fn read_into(lib: &mut Library, cat: &mut Catalogue, of: RemoteList) -> Result<usize, ServiceError> {
        let mut get = |path: &str| Ok(fixture(fixture_path(path)));
        let mut confirmed: Vec<String> = Vec::new();
        let mut after = None;
        let mut dropped = 0;
        loop {
            let page = fetch_page(&mut get, "/v2", "173210380", &of, after.as_ref())?;
            for member in page.members {
                if !confirmed.contains(&member) {
                    confirmed.push(member);
                }
            }
            let reach = if page.next.is_some() { Reach::Partial } else { Reach::Complete };
            let merge = Merge { list: of.clone(), rows: page.rows, confirmed: confirmed.clone(), reach };
            dropped += lib.merge(cat, merge).dropped;
            match page.next {
                Some(next) => after = Some(next),
                None => return Ok(dropped),
            }
        }
    }

    /// A list longer than one page is read to its last page, in order.
    #[test]
    fn liked_tracks_follow_the_cursor_to_the_last_page() {
        let mut cat = Catalogue::seed();
        let mut lib = Library::default();
        read_into(&mut lib, &mut cat, RemoteList::Liked).unwrap();
        let ids: Vec<&str> = lib.liked.iter().map(|t| t.as_str()).collect();
        assert_eq!(ids, ["75413016", "12345678", "98765432"]);
    }

    /// Every fixture decodes into a library the merges keep whole: each member
    /// resolves, and each playlist has its tracks in order.
    #[test]
    fn the_fixtures_decode_into_a_whole_library() {
        let mut cat = Catalogue::seed();
        let mut lib = Library::default();
        let mut dropped = 0;
        for of in [RemoteList::Liked, RemoteList::Albums, RemoteList::Artists, RemoteList::Playlists] {
            dropped += read_into(&mut lib, &mut cat, of).unwrap();
        }
        for raw in [EVENING, MORNING] {
            let id = cat.playlist_id(raw).unwrap();
            dropped += read_into(&mut lib, &mut cat, RemoteList::Playlist(id)).unwrap();
        }
        let counts = (lib.liked.len(), lib.saved_albums.len(), lib.followed.len(), lib.tidal_playlists.len());
        assert_eq!((counts, dropped), ((3, 2, 1, 2), 0));
        let evening = cat.playlist(&cat.playlist_id(EVENING).unwrap()).unwrap();
        assert_eq!(evening.name, "Evening Drive");
        let titles: Vec<&str> =
            evening.tracks.iter().map(|t| cat.track(t).unwrap().title.as_str()).collect();
        assert_eq!(titles, ["Let It Happen", "The Less I Know the Better"]);
        let morning = cat.playlist(&cat.playlist_id(MORNING).unwrap()).unwrap();
        assert_eq!(morning.tracks.len(), 1);
        assert_eq!(lib.check(&cat), Ok(()));
        let cover = |title: &str| {
            let album = cat.albums().find(|a| a.title == title && !a.id.as_str().starts_with("seed-")).unwrap();
            album.cover.as_ref().map(|c| c.as_str().to_owned())
        };
        assert_eq!(cover("Currents").as_deref(), Some("251380836-large"));
        assert_eq!(cover("In Rainbows").as_deref(), Some("108043637-large"));
        assert_eq!(evening.cover.as_ref().map(|c| c.as_str()), Some("playlist-cover-1"));
    }

    /// A catalogue that holds the fixtures' artist and album and none of their
    /// tracks, as when the user opens the album or the artist from Albums.
    fn holding_currents() -> Catalogue {
        let mut cat = Catalogue::seed();
        cat.absorb(CatalogueRows {
            artists: vec![ArtistRow { id: TAME_IMPALA.into(), name: "Tame Impala".into() }],
            albums: vec![AlbumRow {
                id: CURRENTS.into(),
                title: "Currents".into(),
                artist: TAME_IMPALA.into(),
                cover: String::new(),
            }],
            ..CatalogueRows::default()
        });
        cat
    }

    /// Read page one of `of` from the fixtures and merge it as the app does.
    fn merge_first_page(lib: &mut Library, cat: &mut Catalogue, of: RemoteList) -> crate::library::MergeReport {
        let mut get = |path: &str| Ok(fixture(fixture_path(path)));
        let page = fetch_page(&mut get, "/v2", "173210380", &of, None).unwrap();
        assert_eq!(page.next, None, "the fixture has one page");
        lib.merge(cat, Merge { list: of, rows: page.rows, confirmed: page.members, reach: Reach::Complete })
    }

    /// Opening an album reads its tracks into the catalogue, so the album
    /// screen lists them. No library list changes.
    #[test]
    fn an_album_page_brings_its_tracks() {
        let mut cat = holding_currents();
        let mut lib = Library::default();
        let album = cat.album_id(CURRENTS).unwrap();
        assert_eq!(cat.tracks_of_album(&album).count(), 0);

        let report = merge_first_page(&mut lib, &mut cat, RemoteList::Album(album.clone()));
        let titles: Vec<&str> = cat.tracks_of_album(&album).map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["Let It Happen", "The Less I Know the Better"]);
        assert_eq!(report, crate::library::MergeReport { members: 2, dropped: 0 });
        assert_eq!(lib, Library::default(), "an album page changed the library");
    }

    /// Opening an artist reads its albums into the catalogue, so the artist
    /// screen shows its discography. No library list changes.
    #[test]
    fn an_artist_page_brings_its_albums() {
        let mut cat = holding_currents();
        let mut lib = Library::default();
        let artist = cat.artist_id(TAME_IMPALA).unwrap();

        let report = merge_first_page(&mut lib, &mut cat, RemoteList::Artist(artist.clone()));
        let titles: Vec<&str> = cat.albums().filter(|a| a.artist == artist).map(|a| a.title.as_str()).collect();
        assert_eq!(titles, ["Currents", "Let It Happen EP", "The Less I Know the Better"]);
        assert_eq!(report, crate::library::MergeReport { members: 3, dropped: 0 });
        assert_eq!(lib, Library::default(), "an artist page changed the library");
    }

    /// A page whose `next` is the given link, and nothing else.
    fn linking_to(next: &str) -> Document {
        serde_json::from_str(&format!(r#"{{"links":{{"next":"{next}"}}}}"#)).unwrap()
    }

    /// A `next` link that leaves the service is refused before it becomes a
    /// cursor, so the bearer token never goes to another host.
    #[test]
    fn a_next_link_to_another_host_is_refused() {
        for next in ["https://evil.example/v2/x", "//evil.example/v2/x", "/v2//evil.example/x", "x", "v2/x"] {
            let mut asked = Vec::new();
            let result = fetch_page(
                &mut |path: &str| {
                    asked.push(path.to_owned());
                    Ok(linking_to(next))
                },
                "/v2",
                "173210380",
                &RemoteList::Albums,
                None,
            );
            assert!(matches!(result, Err(ServiceError::Decode(_))), "{next} was accepted: {result:?}");
            assert_eq!(asked, [SAVED], "{next} was fetched");
        }
    }

    /// The live service sends `next` from the host root without the base path.
    /// The next page reads it as a path under the base.
    #[test]
    fn a_next_link_without_the_base_path_is_followed() {
        let next = "/userCollectionAlbums/me/relationships/items?include=items&page%5Bcursor%5D=2";
        let mut asked = Vec::new();
        let mut get = |path: &str| {
            asked.push(path.to_owned());
            Ok(if path == SAVED { linking_to(next) } else { serde_json::from_str("{}").unwrap() })
        };
        let first = fetch_page(&mut get, "/v2", "173210380", &RemoteList::Albums, None).unwrap();
        let last = fetch_page(&mut get, "/v2", "173210380", &RemoteList::Albums, first.next.as_ref()).unwrap();
        assert_eq!(last.next, None);
        assert_eq!(asked, [SAVED, next]);
    }

    /// A catalogue playlist id for a Tidal playlist. Only a catalogue mints one.
    fn tidal_playlist(raw: &str) -> crate::catalogue::CataloguePlaylistId {
        let mut cat = crate::catalogue::Catalogue::seed();
        cat.absorb(CatalogueRows {
            playlists: vec![PlaylistRow {
                id: raw.into(),
                name: "Evening Drive".into(),
                description: String::new(),
                tracks: Vec::new(),
                cover: String::new(),
            }],
            ..CatalogueRows::default()
        });
        cat.playlist_id(raw).unwrap()
    }

    fn fixture_body(name: &str) -> String {
        std::fs::read_to_string(format!("{}/tests/fixtures/tidal/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    fn row_ids<T>(rows: &[T], id: impl Fn(&T) -> &str) -> Vec<&str> {
        rows.iter().map(id).collect()
    }

    /// With no pages set, the fake answers each list with one empty last page.
    #[test]
    fn a_list_the_fake_does_not_name_is_one_empty_last_page() {
        let svc = FakeService::default();
        for of in [RemoteList::Liked, RemoteList::Albums, RemoteList::Playlist(tidal_playlist(EVENING))] {
            let page = svc.page("t", "u", &of, None).unwrap();
            assert_eq!(page, Page { rows: CatalogueRows::default(), members: Vec::new(), next: None }, "{of:?}");
        }
    }

    /// Set pages are served in order through the cursors the fake hands out,
    /// and the last page ends the chain. A set error is served as the page.
    #[test]
    fn the_fake_serves_its_pages_in_order_and_the_last_ends_the_chain() {
        let page = |members: &[&str]| {
            Ok(Page { rows: CatalogueRows::default(), members: members.iter().map(|m| m.to_string()).collect(), next: None })
        };
        let svc = FakeService {
            pages: BTreeMap::from([
                (RemoteList::Liked, vec![page(&["a", "b"]), page(&["c"])]),
                (RemoteList::Albums, vec![Err(ServiceError::Http("500".into()))]),
            ]),
            ..FakeService::default()
        };
        let first = svc.page("t", "u", &RemoteList::Liked, None).unwrap();
        assert_eq!(first.members, ["a", "b"]);
        let second = svc.page("t", "u", &RemoteList::Liked, first.next.as_ref()).unwrap();
        assert_eq!(second.members, ["c"]);
        assert_eq!(second.next, None, "the last page did not end the chain");
        assert_eq!(svc.page("t", "u", &RemoteList::Albums, None), Err(ServiceError::Http("500".into())));
    }

    /// A loopback server that answers `count` requests, one per connection, and
    /// logs each path with whether it carried the bearer token, before it
    /// answers, so a test can count requests between calls.
    type RequestLog = Arc<Mutex<Vec<(String, bool)>>>;

    fn serve_logged(
        count: usize,
        answer: impl Fn(&str) -> String + Send + 'static,
    ) -> (String, RequestLog, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let log = Arc::new(Mutex::new(Vec::new()));
        let server_log = log.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..count {
                let (mut conn, _) = listener.accept().unwrap();
                let mut buf = [0u8; 8192];
                let n = conn.read(&mut buf).unwrap();
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = request.split_whitespace().nth(1).unwrap().to_owned();
                let bearer = request.lines().any(|l| l.eq_ignore_ascii_case("authorization: Bearer tok"));
                server_log.lock().unwrap().push((path.clone(), bearer));
                let _ = conn.write_all(answer(&path).as_bytes());
            }
        });
        (base, log, server)
    }

    /// Each call makes one request. The second page is the first page's `next`,
    /// the last page ends the chain, every request carries the token, a `next`
    /// to another host is refused unfetched, and the four lists with covers ask
    /// for them.
    #[test]
    fn http_pages_read_one_response_per_call() {
        let (base, log, server) = serve_logged(8, |path| {
            let relative = path.strip_prefix("/v2").unwrap();
            let body = if relative.starts_with("/userCollectionArtists/") {
                r#"{"data":[],"links":{"next":"https://evil.example/v2/x"}}"#.to_owned()
            } else {
                fixture_body(fixture_path(relative))
            };
            reply("200 OK", "Content-Type: application/vnd.api+json\r\n", &body)
        });
        let svc = HttpService::with_clock(&format!("{base}/v2"), FakeClock::new());
        let requests = || log.lock().unwrap().len();

        let first = svc.page("tok", "173210380", &RemoteList::Liked, None).unwrap();
        assert_eq!(requests(), 1, "one call made more than one request");
        assert_eq!(first.members, ["75413016", "12345678"]);
        assert_eq!(row_ids(&first.rows.tracks, |t| &t.id), ["75413016", "12345678"]);
        assert_eq!(row_ids(&first.rows.albums, |a| &a.id), ["251380836"]);

        let second = svc.page("tok", "173210380", &RemoteList::Liked, first.next.as_ref()).unwrap();
        assert_eq!(requests(), 2, "one call made more than one request");
        assert_eq!(second.members, ["98765432"]);
        assert_eq!(second.next, None, "the last page did not end the chain");

        let cat = holding_currents();
        let (album, artist) = (cat.album_id(CURRENTS).unwrap(), cat.artist_id(TAME_IMPALA).unwrap());
        for of in [
            RemoteList::Albums,
            RemoteList::Playlists,
            RemoteList::Playlist(tidal_playlist(EVENING)),
            RemoteList::Album(album),
            RemoteList::Artist(artist),
        ] {
            svc.page("tok", "173210380", &of, None).unwrap();
        }
        assert_eq!(requests(), 7, "one call made more than one request");
        let refused = svc.page("tok", "173210380", &RemoteList::Artists, None);
        assert!(matches!(refused, Err(ServiceError::Decode(_))), "a foreign next was accepted: {refused:?}");
        server.join().unwrap();

        let log = log.lock().unwrap();
        assert_eq!(log.len(), 8, "a foreign next was fetched");
        assert!(log.iter().all(|(_, bearer)| *bearer), "a request went without the token: {log:?}");
        assert!(log[1].0.ends_with("page%5Bcursor%5D=page-2"), "the second page was not the first page's next: {log:?}");
        let includes = |start: &str| -> Vec<String> {
            let path = &log.iter().find(|(p, _)| p.starts_with(start)).unwrap().0;
            let query = path.split_once("include=").unwrap().1;
            query.split('&').next().unwrap().split(',').map(str::to_owned).collect()
        };
        assert!(includes("/v2/userCollectionTracks/").contains(&"items.albums.coverArt".into()), "{log:?}");
        assert!(includes("/v2/userCollectionAlbums/").contains(&"items.coverArt".into()), "{log:?}");
        assert!(includes("/v2/userCollectionPlaylists/").contains(&"items.coverArt".into()), "{log:?}");
        assert!(includes(&format!("/v2/playlists/{EVENING}/")).contains(&"items.albums.coverArt".into()), "{log:?}");
        assert!(includes("/v2/userCollectionTracks/").contains(&"items.albums.artists".into()), "{log:?}");
        assert!(includes(&format!("/v2/playlists/{EVENING}/")).contains(&"items.albums.artists".into()), "{log:?}");
        assert!(includes(&format!("/v2/albums/{CURRENTS}/relationships/items")).contains(&"items.albums.artists".into()), "{log:?}");
        assert!(includes(&format!("/v2/artists/{TAME_IMPALA}/relationships/albums")).contains(&"albums.artists".into()), "{log:?}");
    }

    /// The playlists list reads the favorited playlists, then the owned ones,
    /// through one cursor chain that ends after the owned page.
    #[test]
    fn playlists_page_through_favorited_then_owned() {
        let mut asked = Vec::new();
        let (favorited, owned_page) = {
            let mut get = |path: &str| {
                asked.push(path.to_owned());
                Ok(fixture(fixture_path(path)))
            };
            let favorited = fetch_page(&mut get, "/v2", "173210380", &RemoteList::Playlists, None).unwrap();
            let owned_page =
                fetch_page(&mut get, "/v2", "173210380", &RemoteList::Playlists, favorited.next.as_ref()).unwrap();
            (favorited, owned_page)
        };
        assert_eq!(favorited.members, [EVENING]);
        assert_eq!(owned_page.members, [EVENING, MORNING]);
        assert_eq!(owned_page.next, None, "the owned page did not end the chain");
        assert_eq!(asked, [FAVORITED.to_owned(), owned("173210380")]);
    }

    /// A `429` on a page call waits one second, asks the same page again, and
    /// the page lands.
    #[test]
    fn a_rate_limited_page_call_is_retried_and_lands() {
        let (base, server) = serve(2, |index, path| match index {
            0 => too_many(),
            _ => reply(
                "200 OK",
                "Content-Type: application/vnd.api+json\r\n",
                &fixture_body(fixture_path(path.strip_prefix("/v2").unwrap())),
            ),
        });
        let clock = FakeClock::new();
        let svc = HttpService::with_clock(&format!("{base}/v2"), clock.clone());
        let page = svc.page("tok", "173210380", &RemoteList::Albums, None).unwrap();
        assert_eq!(page.members, ["251380836", "108043637"]);
        let asked = server.join().unwrap();
        assert_eq!(asked.len(), 2);
        assert_eq!(asked[0], asked[1], "the limited page was not asked again");
        assert_eq!(clock.sleeps()[0], Duration::from_secs(1), "no backoff before the retry");
    }

    /// A cover request for an album or playlist of the fixtures.
    fn cover_request(owner: &str, artwork: &str) -> CoverRequest {
        let mut cat = crate::catalogue::Catalogue::seed();
        cat.absorb(CatalogueRows {
            albums: vec![AlbumRow {
                id: owner.into(),
                title: "Currents".into(),
                artist: "seed-artist-0".into(),
                cover: artwork.into(),
            }],
            ..CatalogueRows::default()
        });
        cat.album(&cat.album_id(owner).unwrap()).unwrap().cover_request().unwrap()
    }

    /// Run `fetch_cover` over one document, recording the paths and the image
    /// URLs it asked for.
    fn read_cover(doc: &str, request: &CoverRequest) -> (Result<Vec<u8>, ServiceError>, Vec<String>, Vec<String>) {
        let mut paths = Vec::new();
        let mut images = Vec::new();
        let result = fetch_cover(
            &mut |path: &str| {
                paths.push(path.to_owned());
                Ok(serde_json::from_str(doc).unwrap())
            },
            &mut |url| {
                images.push(url.to_string());
                Ok(b"image".to_vec())
            },
            request,
        );
        (result, paths, images)
    }

    fn cover_fixture() -> String {
        std::fs::read_to_string(format!("{}/tests/fixtures/tidal/album-cover-art.json", env!("CARGO_MANIFEST_DIR")))
            .unwrap()
    }

    /// The read asks the pinned relationship endpoint, finds the artwork the
    /// relationship names, and fetches one of its files.
    #[test]
    fn a_cover_link_is_read_from_the_artwork_files() {
        let request = cover_request("251380836", "251380836-large");
        let (result, paths, images) = read_cover(&cover_fixture(), &request);
        assert_eq!(result, Ok(b"image".to_vec()));
        assert_eq!(paths, ["/albums/251380836/relationships/coverArt?include=coverArt"]);
        assert_eq!(images, ["https://resources.tidal.com/images/251380836/750x750.jpg"]);
        assert_eq!(
            cover_path(&CoverOwner::Playlist(
                crate::catalogue::Catalogue::seed().playlist_id("seed-playlist-0").unwrap()
            ))
            .as_deref(),
            Some("/playlists/seed-playlist-0/relationships/coverArt?include=coverArt")
        );
    }

    /// Artist cover requests for these Tidal artist ids, in order.
    fn artist_requests(ids: &[String]) -> Vec<CoverRequest> {
        let mut cat = crate::catalogue::Catalogue::seed();
        cat.absorb(CatalogueRows {
            artists: ids.iter().map(|id| ArtistRow { id: id.clone(), name: format!("Artist {id}") }).collect(),
            ..CatalogueRows::default()
        });
        ids.iter().map(|id| cat.artist(&cat.artist_id(id).unwrap()).unwrap().cover_request().unwrap()).collect()
    }

    /// A batched artist read in the live shape: each artist names its artwork
    /// in `relationships.profileArt`, and the artworks are in `included`, in
    /// another order than the artists. `art` maps an artist id to its artwork
    /// id, or to `None` for an empty `profileArt`.
    fn artists_doc(art: &[(&str, Option<String>)]) -> Document {
        let data: Vec<Value> = art
            .iter()
            .map(|(id, artwork)| {
                let named: Vec<Value> = artwork.iter().map(|a| serde_json::json!({ "type": "artworks", "id": a })).collect();
                serde_json::json!({ "type": "artists", "id": id, "attributes": { "name": id },
                    "relationships": { "profileArt": { "data": named } } })
            })
            .collect();
        let included: Vec<Value> = art
            .iter()
            .rev()
            .filter_map(|(id, artwork)| {
                let artwork = artwork.as_ref()?;
                Some(serde_json::json!({ "type": "artworks", "id": artwork, "attributes": { "files": [
                    { "href": format!("https://resources.tidal.com/images/{id}/160x160.jpg"), "meta": { "width": 160 } },
                    { "href": format!("https://resources.tidal.com/images/{id}/750x750.jpg"), "meta": { "width": 750 } },
                ] } }))
            })
            .collect();
        serde_json::from_value(serde_json::json!({ "data": data, "included": included })).unwrap()
    }

    /// Twenty-one artists take two reads: one `filter[id]` read of the first
    /// twenty, then one of the last. Each artist gets the image of the artwork
    /// its own `profileArt` names. A dead id is absent from the answer and an
    /// empty `profileArt` names nothing, so both have no picture and cost no
    /// image read.
    #[test]
    fn a_batched_artist_read_maps_each_picture_to_its_artist() {
        let ids: Vec<String> = (1000..1021).map(|n| n.to_string()).collect();
        let requests = artist_requests(&ids);
        let dead = "1019";
        let bare = "1020";
        let art: Vec<(&str, Option<String>)> = ids
            .iter()
            .filter(|id| id.as_str() != dead)
            .map(|id| (id.as_str(), (id != bare).then(|| format!("{id}-art"))))
            .collect();
        let mut paths = Vec::new();
        let mut images = Vec::new();
        let results = fetch_artist_pictures(
            &mut |path: &str| {
                paths.push(path.to_owned());
                Ok(artists_doc(&art))
            },
            &mut |url| {
                images.push(url.to_string());
                Ok(url.to_string().into_bytes())
            },
            &requests,
        );

        let first: Vec<&str> = ids[..20].iter().map(String::as_str).collect();
        assert_eq!(
            paths,
            [
                format!("/artists?filter%5Bid%5D={}&include=profileArt", first.join("%2C")),
                "/artists?filter%5Bid%5D=1020&include=profileArt".to_string(),
            ]
        );
        assert_eq!(results.len(), ids.len(), "a request got no result");
        for ((artwork, result), id) in results.iter().zip(&ids) {
            assert_eq!(artwork.as_str(), format!("artist-{id}"), "a result is out of request order");
            let expected = if id == dead || id == bare {
                Err(ServiceError::NoPicture)
            } else {
                Ok(format!("https://resources.tidal.com/images/{id}/750x750.jpg").into_bytes())
            };
            assert_eq!(result, &expected, "artist {id} got the wrong picture");
        }
        assert_eq!(images.len(), 19, "an artist with no picture made an image read");
    }

    /// A failed batched read fails every artist of its chunk with that error,
    /// so a rate limit defers them all rather than calling them pictureless.
    #[test]
    fn a_failed_batched_read_fails_its_whole_chunk() {
        let ids: Vec<String> = (2000..2003).map(|n| n.to_string()).collect();
        let results = fetch_artist_pictures(
            &mut |_: &str| Err(ServiceError::RateLimited("429".to_string())),
            &mut |_| panic!("an image was read after a failed read"),
            &artist_requests(&ids),
        );
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|(_, r)| r == &Err(ServiceError::RateLimited("429".to_string()))));
    }

    /// A single artist cover goes through the same batched read.
    #[test]
    fn a_single_artist_cover_uses_the_batched_read() {
        let request = artist_requests(&["1566".to_string()]).remove(0);
        let doc = r#"{"data":[{"type":"artists","id":"1566","attributes":{},
            "relationships":{"profileArt":{"data":[{"type":"artworks","id":"a1b2c3d4"}]}}}],
            "included":[{"type":"artworks","id":"a1b2c3d4","attributes":{"files":[
            {"href":"https://resources.tidal.com/images/a1b2c3d4/750x750.jpg","meta":{"width":750}}]}}]}"#;
        let (result, paths, images) = read_cover(doc, &request);
        assert_eq!(result, Ok(b"image".to_vec()));
        assert_eq!(paths, ["/artists?filter%5Bid%5D=1566&include=profileArt"]);
        assert_eq!(images, ["https://resources.tidal.com/images/a1b2c3d4/750x750.jpg"]);
    }

    /// The smallest file at least `COVER_PX` wide wins. With none that wide,
    /// the widest wins.
    #[test]
    fn the_smallest_file_at_or_above_the_width_wins() {
        let file = |w: u64| serde_json::json!({ "href": format!("https://i.example/{w}.jpg"), "meta": { "width": w } });
        let attrs = |ws: &[u64]| serde_json::json!({ "files": ws.iter().map(|&w| file(w)).collect::<Vec<_>>() });
        assert_eq!(pick_file(Some(&attrs(&[1280, 160, 640, 750]))).as_deref(), Some("https://i.example/640.jpg"));
        assert_eq!(pick_file(Some(&attrs(&[1280, 750]))).as_deref(), Some("https://i.example/750.jpg"));
        assert_eq!(pick_file(Some(&attrs(&[80, 320, 160]))).as_deref(), Some("https://i.example/320.jpg"));
    }

    /// A link that is not an absolute https URL is refused before any image
    /// request, so a cover never comes over plain HTTP or from a relative path.
    #[test]
    fn a_cover_link_that_is_not_https_is_refused() {
        let request = cover_request("251380836", "251380836-large");
        for href in ["http://resources.tidal.com/x.jpg", "/images/x.jpg", "//resources.tidal.com/x.jpg", "file:///etc/passwd", "ftp://resources.tidal.com/x.jpg"] {
            let doc = serde_json::json!({
                "included": [{ "type": "artworks", "id": "251380836-large",
                    "attributes": { "files": [{ "href": href, "meta": { "width": 640 } }] } }]
            })
            .to_string();
            let (result, _, images) = read_cover(&doc, &request);
            assert!(matches!(result, Err(ServiceError::Decode(_))), "{href} was accepted: {result:?}");
            assert!(images.is_empty(), "{href} was fetched");
        }
    }

    /// An artwork with no usable file, or a response without the named artwork,
    /// is a decode error, not an empty cover.
    #[test]
    fn a_cover_with_no_files_is_a_decode_error() {
        let request = cover_request("251380836", "251380836-large");
        for doc in [
            r#"{"included":[{"type":"artworks","id":"251380836-large","attributes":{"files":[]}}]}"#,
            r#"{"included":[{"type":"artworks","id":"251380836-large","attributes":{}}]}"#,
            r#"{"included":[{"type":"artworks","id":"251380836-large","attributes":{"files":[{"href":"https://i.example/x.jpg"}]}}]}"#,
            r#"{"included":[{"type":"artworks","id":"someone-else","attributes":{"files":[{"href":"https://i.example/x.jpg","meta":{"width":640}}]}}]}"#,
            r#"{"data":[]}"#,
        ] {
            let (result, _, images) = read_cover(doc, &request);
            assert!(matches!(result, Err(ServiceError::Decode(_))), "{doc} gave {result:?}");
            assert!(images.is_empty());
        }
    }

    /// The image request carries no bearer token, and a body over the cap is
    /// refused rather than read whole.
    #[test]
    fn an_image_body_over_the_cap_is_refused() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut authorized = Vec::new();
            for size in [16usize, MAX_COVER_BYTES as usize + 1] {
                let (mut conn, _) = listener.accept().unwrap();
                let mut buf = [0u8; 8192];
                let n = conn.read(&mut buf).unwrap();
                let request = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                authorized.push(request.contains("authorization:"));
                let _ = conn.write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n").as_bytes(),
                );
                let _ = conn.write_all(&vec![0xAB; size]);
            }
            authorized
        });
        let svc = HttpService::with_clock(&format!("http://127.0.0.1:{}/v2", addr.port()), FakeClock::new());
        let url = reqwest::Url::parse(&format!("http://127.0.0.1:{}/image.jpg", addr.port())).unwrap();
        assert_eq!(svc.get_image(&url), Ok(vec![0xAB; 16]));
        assert!(matches!(svc.get_image(&url), Err(ServiceError::Http(_))), "an over-cap body was accepted");
        assert_eq!(server.join().unwrap(), [false, false], "an image request carried a token");
    }

    /// The fake answers from its cover map and counts every call.
    #[test]
    fn fake_service_serves_and_counts_covers() {
        let svc = FakeService {
            covers: BTreeMap::from([("251380836-large".to_string(), Ok(b"png".to_vec()))]),
            ..FakeService::default()
        };
        assert_eq!(svc.cover("t", &cover_request("251380836", "251380836-large")), Ok(b"png".to_vec()));
        assert_eq!(svc.cover("t", &cover_request("251380836", "other")), Err(ServiceError::Http("404".to_string())));
        assert_eq!(svc.cover_calls.load(Ordering::SeqCst), 2);
    }

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

    /// An account email is not a display name. Tidal returns it in `username`,
    /// and the name is written to disk in the clear.
    #[test]
    fn an_email_is_not_used_as_a_name() {
        let attrs = serde_json::json!({ "username": "ada@example.com" });
        assert_eq!(name_from(&Some(attrs)), "You");

        let attrs = serde_json::json!({ "firstName": "Ada", "username": "ada@example.com" });
        assert_eq!(name_from(&Some(attrs)), "Ada");

        assert!(is_email("ada@example.com"));
        assert!(!is_email("ada"));
        assert!(!is_email("@example.com"));
        assert!(!is_email("ada@"));
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

    /// A transient failure keeps the token; only a `400`/`401` naming
    /// `invalid_grant` is permanent.
    #[test]
    fn token_error_distinguishes_invalid_grant() {
        use reqwest::StatusCode;
        assert!(matches!(
            token_error(StatusCode::from_u16(503).unwrap(), "unavailable"),
            ServiceError::Auth(_)
        ));
        assert!(matches!(
            token_error(StatusCode::BAD_REQUEST, r#"{"error":"invalid_grant"}"#),
            ServiceError::InvalidGrant(_)
        ));
        assert!(matches!(
            token_error(StatusCode::UNAUTHORIZED, r#"{"error":"invalid_grant"}"#),
            ServiceError::InvalidGrant(_)
        ));
        assert!(matches!(
            token_error(StatusCode::BAD_REQUEST, r#"{"error":"invalid_client"}"#),
            ServiceError::Auth(_)
        ));
    }

    /// An accepted connection carries read and write timeouts, so a client that
    /// connects and stays silent cannot block the worker.
    #[test]
    fn accepted_stream_has_io_timeouts() {
        use std::net::{TcpListener, TcpStream};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::thread::spawn(move || {
            let _conn = TcpStream::connect(addr).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(200));
        });
        let stream = accept_with_timeout(&listener).unwrap();
        assert!(stream.read_timeout().unwrap().is_some());
        assert!(stream.write_timeout().unwrap().is_some());
        client.join().unwrap();
    }

    /// A `401` from `/users/me` surfaces as [`ServiceError::Unauthorized`], so
    /// the caller refreshes and retries rather than keeping the fallback name.
    #[test]
    fn current_user_propagates_unauthorized() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = conn.read(&mut buf);
            let _ = conn.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
        });
        let svc = HttpService::new(&format!("http://127.0.0.1:{}/v2", addr.port()));
        let payload = URL_SAFE_NO_PAD.encode(br#"{"uid":"42"}"#);
        let token = format!("h.{payload}.sig");
        assert_eq!(svc.current_user(&token), Err(ServiceError::Unauthorized));
        server.join().unwrap();
    }

    /// The name always comes from `/users/me`, never from a name claim the JWT
    /// happens to carry, so a revoked token is still rejected rather than shown
    /// from the token alone.
    #[test]
    fn current_user_ignores_jwt_name_and_fetches_profile() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = conn.read(&mut buf);
            let body = r#"{"data":{"type":"users","attributes":{"username":"server-name"}}}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.api+json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = conn.write_all(resp.as_bytes());
        });
        let svc = HttpService::new(&format!("http://127.0.0.1:{}/v2", addr.port()));
        let payload = URL_SAFE_NO_PAD.encode(br#"{"uid":"42","username":"jwt-name"}"#);
        let token = format!("h.{payload}.sig");
        assert_eq!(svc.current_user(&token).unwrap().name, "server-name");
        server.join().unwrap();
    }

    /// A clock that moves only when told to, and records every sleep.
    struct FakeClock {
        start: Instant,
        elapsed: Mutex<Duration>,
        sleeps: Mutex<Vec<Duration>>,
    }

    impl FakeClock {
        fn new() -> Arc<Self> {
            Arc::new(FakeClock { start: Instant::now(), elapsed: Mutex::default(), sleeps: Mutex::default() })
        }
        fn advance(&self, by: Duration) {
            *self.elapsed.lock().unwrap() += by;
        }
        fn sleeps(&self) -> Vec<Duration> {
            self.sleeps.lock().unwrap().clone()
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            self.start + *self.elapsed.lock().unwrap()
        }
        fn sleep(&self, duration: Duration) {
            self.advance(duration);
            self.sleeps.lock().unwrap().push(duration);
        }
    }

    /// A loopback server that answers `count` requests, one per connection,
    /// with `answer(index, path)`, and returns the paths it was asked for.
    fn serve(
        count: usize,
        answer: impl Fn(usize, &str) -> String + Send + 'static,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let server = std::thread::spawn(move || {
            let mut asked = Vec::new();
            for index in 0..count {
                let (mut conn, _) = listener.accept().unwrap();
                let mut buf = [0u8; 8192];
                let n = conn.read(&mut buf).unwrap();
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = request.split_whitespace().nth(1).unwrap().to_owned();
                let _ = conn.write_all(answer(index, &path).as_bytes());
                asked.push(path);
            }
            asked
        });
        (base, server)
    }

    fn reply(status: &str, headers: &str, body: &str) -> String {
        format!("HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    }

    fn too_many() -> String {
        reply("429 Too Many Requests", "", "")
    }

    /// A page that is limited every time gives up after four retries with a
    /// doubling backoff, and the read returns `RateLimited` and no page.
    #[test]
    fn a_page_limited_every_time_fails_after_the_cap() {
        let (base, server) = serve(5, |_, _| too_many());
        let clock = FakeClock::new();
        let svc = HttpService::with_clock(&format!("{base}/v2"), clock.clone());
        let page = svc.page("tok", "173210380", &RemoteList::Liked, None);
        assert!(matches!(page, Err(ServiceError::RateLimited(_))), "a limited read gave {page:?}");
        assert_eq!(server.join().unwrap().len(), 5, "the read did not stop after four retries");
        let secs: Vec<u64> = clock.sleeps().iter().map(Duration::as_secs).collect();
        assert_eq!(secs, [1, 2, 4, 8]);
    }

    /// A `Retry-After` header sets the wait before the retry.
    #[test]
    fn a_retry_after_header_sets_the_wait() {
        let (base, server) = serve(2, |index, _| match index {
            0 => reply("429 Too Many Requests", "Retry-After: 7\r\n", ""),
            _ => reply("200 OK", "Content-Type: application/vnd.api+json\r\n", r#"{"data":[]}"#),
        });
        let clock = FakeClock::new();
        let svc = HttpService::with_clock(&format!("{base}/v2"), clock.clone());
        assert!(svc.get_doc("tok", "/users/me").is_ok());
        server.join().unwrap();
        assert_eq!(clock.sleeps(), [Duration::from_secs(7)]);
    }

    /// Requests start at least the minimum gap apart, counting the time the
    /// previous request took. Cover images go through the same pacer.
    #[test]
    fn the_pacer_keeps_its_minimum_gap() {
        let clock = FakeClock::new();
        let server_clock = clock.clone();
        let started = Arc::new(Mutex::new(Vec::new()));
        let seen = started.clone();
        let (base, server) = serve(3, move |_, _| {
            seen.lock().unwrap().push(server_clock.now());
            server_clock.advance(Duration::from_millis(100));
            reply("200 OK", "", "{}")
        });
        let svc = HttpService::with_clock(&format!("{base}/v2"), clock.clone());
        for _ in 0..3 {
            assert!(svc.get_doc("tok", "/users/me").is_ok());
        }
        server.join().unwrap();
        let started = started.lock().unwrap();
        let gaps: Vec<Duration> = started.windows(2).map(|w| w[1] - w[0]).collect();
        assert_eq!(gaps, [MIN_GAP, MIN_GAP], "requests went closer than the minimum gap");
    }

    /// Image reads go to the image host, which the pacer does not guard, so
    /// back-to-back image reads never wait, even right after a paced read.
    #[test]
    fn an_image_read_is_not_paced() {
        let clock = FakeClock::new();
        let (base, server) = serve(4, |_, _| reply("200 OK", "", "{}"));
        let svc = HttpService::with_clock(&format!("{base}/v2"), clock.clone());
        assert!(svc.get_doc("tok", "/users/me").is_ok());
        let url = reqwest::Url::parse(&format!("{base}/image.jpg")).unwrap();
        for _ in 0..3 {
            assert_eq!(svc.get_image(&url), Ok(b"{}".to_vec()));
        }
        server.join().unwrap();
        assert_eq!(clock.sleeps(), [] as [Duration; 0], "an image read waited on the pacer");
    }
}
