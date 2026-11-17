# Releasing OpenTidal

A checklist for turning the dev build into a shipped macOS app that a user can
sign into without a keychain prompt.

## 1. Sign the app

The keychain prompt in the dev build comes from the unsigned binary. macOS binds
each keychain item's access list to the app's code signature. An unsigned binary
has no stable identity, so the OS asks every time. Signing fixes this. Do not
use the dev `-A` "allow all apps" trick in a release.

- Get an Apple Developer ID certificate for direct distribution, or an App
  Store provisioning profile.
- Give the app a stable bundle id, for example `com.example.opentidal`.
- Sign the bundle:

  ```sh
  codesign --force --deep --sign "Developer ID Application: <Team> (<TeamId>)" OpenTidal.app
  ```

The access list then binds to that identity. The app reads and writes its own
token without prompting, across updates.

## 2. Bundle as an .app

- Build a proper `.app` with an Info.plist.
- The Info.plist needs `CFBundleIdentifier` and a URL scheme for the OAuth
  redirect, so the browser can hand the callback back to the app.
- The loopback redirect (`http://127.0.0.1:PORT/callback`) works today. A custom
  scheme is the alternative if Tidal changes its redirect policy.

## 3. Keychain storage

- Tokens live in the macOS keychain under service `opentidal`, account `tidal`.
- Prefer `kSecAttrAccessibleAfterFirstUnlock`, so the app can read the token at
  launch while the keychain is still locked. `keyring` does not expose this.
  Switching the store to `security-framework` is the follow-up when launch reads
  matter.

## 4. Credentials and live mode

The dev build reads live mode and credentials from env vars. A shipped `.app`
launched by double-click has no environment, so it reads them from a config
file instead. The file is the operator's and stays out of the repo.

- Live mode: `OPENTIDAL_LIVE=1` (env), or `"live": true` (config file).
- Credentials: `TIDAL_CLIENT_ID`, `TIDAL_CLIENT_SECRET`, `TIDAL_REDIRECT_URI`,
  `TIDAL_SCOPE` (env), or the lowercase keys `client_id`, `client_secret`,
  `redirect_uri`, `scope` in the config file. The env var wins
  when both are set. `TIDAL_SCOPE` defaults to every read scope when left unset.
- Config file: name it with `OPENTIDAL_CONFIG`, or leave it at the default path
  `~/Library/Application Support/OpenTidal/config.json` (macOS) /
  `~/.config/opentidal/config.json` elsewhere. The file holds the client secret
  in plaintext, so set `chmod 600` and keep it out of the repo:

  ```json
  {
    "live": true,
    "client_id": "<client id>",
    "client_secret": "<client secret>",
    "redirect_uri": "http://127.0.0.1:8080/callback"
  }
  ```

  Omit `scope` to request every read scope the app needs. A hard-coded scope
  list drifts from [`DEFAULT_SCOPE`](../src/auth.rs) and from the Tidal
  documentation. The list changed twice already.
- The client id and secret never enter the repo. The user signs in once through
  the browser, and the refresh token lands in the keychain. On later launches
  the app restores the session and refreshes an expired access token
  automatically, so the user signs in only when the refresh token is revoked.

## 5. Verify before shipping

- `cargo test` — the invariant proofs and the auth and service tests.
- `scripts/control-opentidal doctor` — one readiness check.
- `scripts/control-opentidal drive features/sign-in/script.json artifacts/sign-in`
  — the sign-in journey against the fake.
- Live check with a real account: set `OPENTIDAL_LIVE=1` and the `TIDAL_*` env
  vars, sign in, and confirm the name shows in the sidebar.

## 6. The API surface, for reference

- The media type is `application/vnd.api+json`.
- The user id comes from the token's JWT `sub` claim, not from `/users/me`.
- Liked tracks: `GET /userCollectionTracks/me/relationships/items?include=items`,
  paged by `page[cursor]`.
- `docs/tidal-api.md` holds the plan for the remaining catalogue and library
  work.

## 7. Distribution

- Direct distribution: notarize the signed `.app` and ship a dmg or zip.

  ```sh
  xcrun notarytool submit OpenTidal.dmg --apple-id <id> --team-id <id> --wait
  xcrun stapler staple OpenTidal.dmg
  ```

- App Store: use the App Store profile and Transporter.
