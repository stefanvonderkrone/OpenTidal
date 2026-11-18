//! The Tidal boundary: the remote data types, the [`MusicService`] port, and
//! its two impls. [`FakeService`] is deterministic and backs every proof and
//! headless drive. [`HttpService`] is a thin blocking `reqwest` client over the
//! Tidal JSON:API.
//!
//! This module is rune-free. It holds no network or token state in `State`; the
//! app wraps a service in `Arc` on [`crate::app::App`].

use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::Value;

use crate::auth::{OAuthConfig, Token};
use crate::catalogue::{AlbumRow, ArtistRow, CatalogueRows, PlaylistRow, TrackRow};
use crate::library::LibraryRows;

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
}

/// The read-only music port. Every method takes the access token, so the fake
/// ignores it and the real client sends it as a bearer header.
pub trait MusicService: Send + Sync {
    fn current_user(&self, token: &str) -> Result<User, ServiceError>;
    /// Read every page of the account's library. A failed page, a repeated
    /// cursor, or the page cap returns an error and no rows.
    fn library(&self, token: &str, user: &str) -> Result<LibraryRows, ServiceError>;
}

/// A fixed, deterministic service for tests and headless drives.
pub struct FakeService {
    pub user: User,
    /// What a sync reads. The default is an empty library.
    pub library: Result<LibraryRows, ServiceError>,
}

impl Default for FakeService {
    fn default() -> Self {
        FakeService {
            user: User { id: UserId("fake-user".to_string()), name: "Fake User".to_string() },
            library: Ok(LibraryRows::default()),
        }
    }
}

impl MusicService for FakeService {
    fn current_user(&self, _token: &str) -> Result<User, ServiceError> {
        Ok(self.user.clone())
    }
    fn library(&self, _token: &str, _user: &str) -> Result<LibraryRows, ServiceError> {
        self.library.clone()
    }
}

/// The real client over the Tidal JSON:API (`https://openapi.tidal.com/v2`).
pub struct HttpService {
    client: reqwest::blocking::Client,
    base: String,
    /// The path of `base`, such as `/v2`. A `next` link starts with it.
    prefix: String,
}

impl HttpService {
    pub fn new(base: &str) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("http client");
        let base = base.trim_end_matches('/').to_string();
        let prefix = reqwest::Url::parse(&base)
            .map(|url| url.path().trim_end_matches('/').to_string())
            .unwrap_or_default();
        HttpService { client, base, prefix }
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

    fn library(&self, token: &str, user: &str) -> Result<LibraryRows, ServiceError> {
        fetch_library(&mut |path: &str| self.get_doc(token, path), &self.prefix, user)
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
                cover: String::new(),
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
                cover: String::new(),
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

/// The page budget for one whole library read, shared by every list. At about
/// twenty items a page this is some 8000 items.
const MAX_PAGES: usize = 400;

/// One page source: a path under the service base in, a document out.
type PageSource<'a> = dyn FnMut(&str) -> Result<Document, ServiceError> + 'a;

/// Follow `links.next` from `first` until it is absent, handing each page to
/// `each`. Every page spends one unit of `budget`. A cursor this walk already
/// followed, an empty budget, or a failed page ends the walk with an error.
fn walk(
    budget: &mut usize,
    prefix: &str,
    get: &mut PageSource,
    first: &str,
    each: &mut dyn FnMut(Document),
) -> Result<(), ServiceError> {
    let mut seen = BTreeSet::new();
    let mut path = first.to_owned();
    loop {
        if !seen.insert(path.clone()) {
            return Err(ServiceError::Paging("repeated cursor".to_string()));
        }
        if *budget == 0 {
            return Err(ServiceError::Paging("page cap".to_string()));
        }
        *budget -= 1;
        let doc = get(&path)?;
        let next = doc.links.next.clone();
        each(doc);
        match next {
            None => return Ok(()),
            Some(next) => path = under_base(prefix, &next)?,
        }
    }
}

/// A `next` link as a path under the service base. Tidal sends it from the host
/// root, such as `/v2/...` for the base `https://openapi.tidal.com/v2`. A link
/// that is not a path under the base is refused, so the bearer token never
/// leaves the service.
fn under_base(prefix: &str, next: &str) -> Result<String, ServiceError> {
    next.strip_prefix(prefix)
        .filter(|rest| rest.starts_with('/') && !rest.starts_with("//"))
        .map(str::to_owned)
        .ok_or_else(|| ServiceError::Decode(format!("a next link leaves the service: {next}")))
}

const LIKED: &str = "/userCollectionTracks/me/relationships/items?include=items,items.albums,items.artists";
const SAVED: &str = "/userCollectionAlbums/me/relationships/items?include=items,items.artists";
const FOLLOWED: &str = "/userCollectionArtists/me/relationships/items?include=items";
const FAVORITED: &str = "/userCollectionPlaylists/me/relationships/items?include=items";

/// The account's playlists, by owner. The brackets are percent-encoded, the
/// way Tidal writes them in its own links.
fn owned(user: &str) -> String {
    format!("/playlists?filter%5Bowners.id%5D={user}")
}

fn playlist_items(id: &str) -> String {
    format!("/playlists/{id}/relationships/items?include=items,items.albums,items.artists")
}

/// The whole library read, over any page source. `HttpService` passes its
/// `get_doc`, and a test passes a map from path to fixture. `prefix` is the
/// path of the service base, which `next` links start with. The rows exist only
/// when every page of every list arrived.
fn fetch_library(get: &mut PageSource, prefix: &str, user: &str) -> Result<LibraryRows, ServiceError> {
    let mut budget = MAX_PAGES;
    let mut rows = LibraryRows::default();
    type Members = fn(&mut LibraryRows) -> &mut Vec<String>;
    let lists: [(String, &str, Members); 5] = [
        (LIKED.to_owned(), "tracks", |r| &mut r.liked),
        (SAVED.to_owned(), "albums", |r| &mut r.saved_albums),
        (FOLLOWED.to_owned(), "artists", |r| &mut r.followed),
        (FAVORITED.to_owned(), "playlists", |r| &mut r.tidal_playlists),
        (owned(user), "playlists", |r| &mut r.tidal_playlists),
    ];
    for (first, kind, members) in lists {
        walk(&mut budget, prefix, get, &first, &mut |doc| {
            members(&mut rows).extend(doc.members(kind));
            doc.add_to(&mut rows.catalogue);
        })?;
    }
    let mut distinct = BTreeSet::new();
    let playlists: Vec<String> =
        rows.tidal_playlists.iter().filter(|id| distinct.insert(*id)).cloned().collect();
    for id in playlists {
        let mut tracks = Vec::new();
        walk(&mut budget, prefix, get, &playlist_items(&id), &mut |doc| {
            tracks.extend(doc.members("tracks"));
            doc.add_to(&mut rows.catalogue);
        })?;
        for row in rows.catalogue.playlists.iter_mut().filter(|p| p.id == id) {
            row.tracks = tracks.clone();
        }
    }
    Ok(rows)
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

    /// A synthetic fixture from `tests/fixtures/tidal`, decoded the way
    /// `get_doc` decodes a response body.
    fn fixture(name: &str) -> Document {
        let path = format!("{}/tests/fixtures/tidal/{name}", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
    }

    const EVENING: &str = "f6b9f0a1-6c8e-4d24-9c71-1d0e2f3a4b5c";
    const MORNING: &str = "0c1d2e3f-4a5b-6c7d-8e9f-a0b1c2d3e4f5";

    /// The fixture for each path the read asks for. A path with no fixture
    /// panics, so a read that asks for something unexpected names it.
    fn fixture_path(path: &str) -> &'static str {
        let liked_2 = format!("{LIKED}&page%5Bcursor%5D=page-2");
        match path {
            p if p == LIKED => "liked-tracks-page-1.json",
            p if p == liked_2 => "liked-tracks-page-2.json",
            p if p == SAVED => "saved-albums.json",
            p if p == FOLLOWED => "followed-artists.json",
            p if p == FAVORITED => "playlists.json",
            p if p == owned("173210380") => "owned-playlists.json",
            p if p == playlist_items(EVENING) => "playlist-items.json",
            p if p == playlist_items(MORNING) => "owned-playlist-items.json",
            other => panic!("the read asked for a path with no fixture: {other}"),
        }
    }

    /// The whole fixture library, read through `fetch_library`, with every path
    /// it asked for in order.
    fn read_fixtures(
        mut fail: impl FnMut(&str) -> Option<ServiceError>,
    ) -> (Result<LibraryRows, ServiceError>, Vec<String>) {
        let mut asked = Vec::new();
        let result = fetch_library(
            &mut |path: &str| {
                asked.push(path.to_owned());
                match fail(path) {
                    Some(error) => Err(error),
                    None => Ok(fixture(fixture_path(path))),
                }
            },
            "/v2",
            "173210380",
        );
        (result, asked)
    }

    /// A list longer than one page is read to its last page, in order.
    #[test]
    fn liked_tracks_follow_the_cursor_to_the_last_page() {
        let (rows, _) = read_fixtures(|_| None);
        assert_eq!(rows.unwrap().liked, ["75413016", "12345678", "98765432"]);
    }

    /// Every fixture decodes into a library the sync keeps whole: each member
    /// resolves, and each playlist has its tracks in order.
    #[test]
    fn the_fixtures_decode_into_a_whole_library() {
        let (rows, asked) = read_fixtures(|_| None);
        assert_eq!(
            asked.iter().filter(|p| p.contains(EVENING)).count(),
            1,
            "a playlist both owned and favorited was read twice"
        );
        let mut cat = crate::catalogue::Catalogue::seed();
        let mut lib = crate::library::Library::default();
        let report = lib.sync(&mut cat, rows.unwrap());
        assert_eq!(
            report,
            crate::library::SyncReport { liked: 3, saved_albums: 2, followed: 1, tidal_playlists: 2, dropped: 0 }
        );
        let evening = cat.playlist(&cat.playlist_id(EVENING).unwrap()).unwrap();
        assert_eq!(evening.name, "Evening Drive");
        let titles: Vec<&str> =
            evening.tracks.iter().map(|t| cat.track(t).unwrap().title.as_str()).collect();
        assert_eq!(titles, ["Let It Happen", "The Less I Know the Better"]);
        let morning = cat.playlist(&cat.playlist_id(MORNING).unwrap()).unwrap();
        assert_eq!(morning.tracks.len(), 1);
        assert_eq!(lib.check(&cat), Ok(()));
    }

    /// A page that fails fails the whole read. No rows exist, so nothing
    /// partial can be applied.
    #[test]
    fn a_failed_page_fails_the_whole_fetch() {
        let (rows, _) = read_fixtures(|path| {
            path.contains("page-2").then(|| ServiceError::Http("429 Too Many Requests".to_string()))
        });
        assert!(matches!(rows, Err(ServiceError::Http(_))), "a failed page was skipped: {rows:?}");
        let (rows, _) = read_fixtures(|path| {
            (path == playlist_items(MORNING)).then(|| ServiceError::Http("500".to_string()))
        });
        assert!(matches!(rows, Err(ServiceError::Http(_))), "a failed playlist page was skipped: {rows:?}");
    }

    /// A page whose `next` is a page from this walk ends the read with an error,
    /// not a loop.
    #[test]
    fn a_repeated_cursor_fails_the_fetch() {
        let mut calls = 0;
        let mut budget = MAX_PAGES;
        let result = walk(
            &mut budget,
            "/v2",
            &mut |path: &str| {
                calls += 1;
                assert!(calls <= MAX_PAGES, "the walk ran past the page cap");
                let next = if path == "/a" { "/v2/b" } else { "/v2/a" };
                Ok(serde_json::from_str(&format!(r#"{{"links":{{"next":"{next}"}}}}"#)).unwrap())
            },
            "/a",
            &mut |_| {},
        );
        assert_eq!(result, Err(ServiceError::Paging("repeated cursor".to_string())));
        assert_eq!(calls, 2);
    }

    /// A source with no last page ends at the cap. The source panics past the
    /// cap, so a broken cap fails the test instead of hanging it.
    #[test]
    fn the_page_cap_fails_the_fetch() {
        let mut calls = 0;
        let mut budget = MAX_PAGES;
        let result = walk(
            &mut budget,
            "/v2",
            &mut |_: &str| {
                calls += 1;
                assert!(calls <= MAX_PAGES, "the walk ran past the page cap");
                Ok(serde_json::from_str(&format!(r#"{{"links":{{"next":"/v2/p?cursor={calls}"}}}}"#)).unwrap())
            },
            "/p",
            &mut |_| {},
        );
        assert_eq!(result, Err(ServiceError::Paging("page cap".to_string())));
        assert_eq!(calls, MAX_PAGES);
    }

    /// A `next` link that leaves the service is refused before it is fetched,
    /// so the bearer token never goes to another host.
    #[test]
    fn a_next_link_to_another_host_is_refused() {
        for next in ["https://evil.example/v2/x", "//evil.example/v2/x", "/v2//evil.example/x", "/x"] {
            let mut asked = Vec::new();
            let mut budget = MAX_PAGES;
            let result = walk(
                &mut budget,
                "/v2",
                &mut |path: &str| {
                    asked.push(path.to_owned());
                    Ok(serde_json::from_str(&format!(r#"{{"links":{{"next":"{next}"}}}}"#)).unwrap())
                },
                "/first",
                &mut |_| {},
            );
            assert!(matches!(result, Err(ServiceError::Decode(_))), "{next} was followed: {result:?}");
            assert_eq!(asked, ["/first"], "{next} was fetched");
        }
    }

    /// The real client reads every page over HTTP, follows `next` under the
    /// base path, and sends the bearer token on every request.
    #[test]
    fn http_library_reads_through_a_loopback_server() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        // One request per connection, closed after the answer, so the server can
        // read each request whole. Eight pages make the whole fixture library.
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..8 {
                let (mut conn, _) = listener.accept().unwrap();
                let mut buf = [0u8; 8192];
                let n = conn.read(&mut buf).unwrap();
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = request.split_whitespace().nth(1).unwrap().to_owned();
                let bearer = request
                    .lines()
                    .any(|l| l.eq_ignore_ascii_case("authorization: Bearer tok"));
                let relative = path.strip_prefix("/v2").unwrap();
                let body = std::fs::read_to_string(format!(
                    "{}/tests/fixtures/tidal/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    fixture_path(relative)
                ))
                .unwrap();
                let _ = conn.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.api+json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
                seen.push((path, bearer));
            }
            seen
        });
        let svc = HttpService::new(&format!("http://127.0.0.1:{}/v2", addr.port()));
        let rows = svc.library("tok", "173210380").unwrap();
        let seen = server.join().unwrap();
        assert_eq!(rows.liked, ["75413016", "12345678", "98765432"]);
        assert_eq!(seen.len(), 8);
        assert!(seen.iter().all(|(_, bearer)| *bearer), "a request went without the token: {seen:?}");
        assert!(seen[1].0.ends_with("page%5Bcursor%5D=page-2"), "the second page was not requested: {seen:?}");
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
}
