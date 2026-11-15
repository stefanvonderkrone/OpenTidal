# Agent rules

Every rule here exists because the same mistake happened twice. Each one names
what enforces it, so a rule with no enforcement is a known repeat.

Run `scripts/preflight.sh` before every commit. It runs the tests, runs clippy
on this crate with warnings as errors, and checks the rules marked below.

| Rule | Enforced by |
| --- | --- |
| Leave `TIDAL_SCOPE` unset in docs and scripts. The list lives in `DEFAULT_SCOPE` only. | `scripts/preflight.sh`, which fails when the literal scope string appears outside `src/auth.rs` |
| A `Library` field is stored by `src/store.rs` and checked by `Library::check`, or it is not part of the library. | `cargo test`. The round-trip test compares whole libraries, so a new field with no column fails it. |
| The library is on disk before the app rests or quits. | `cargo test`, `only_a_dirty_library_asks_for_a_frame` and `a_quit_writes_what_is_outstanding` |
| A session that can save has both a user id and an open database. | `open_library` takes the id, so no caller can set one without the other. |
| The display name lives in `Library`. Do not add a second copy to `Auth` or to a screen. | `cargo test`. The restart test asserts the name comes back from the file. |
| A store that cannot be read stays closed for writing. | `cargo test`, `a_broken_file_is_never_overwritten` |
| Every `.rs` file stays rune-free above `screens.rs` and `main.rs`. | Review. `src/store.rs`, `src/library.rs`, `src/state.rs`, `src/auth.rs`, and `src/tidal.rs` must not import `rune`. |
| Tests that claim a user-visible outcome must be proven able to fail. Break the code, watch the test fail, revert. | Review. A test whose first version passed for the wrong reason is called out in its commit message. |
| One name per thing. `Store::write` writes the library, **save** means an album, the keychain is `tokens`. | `GLOSSARY.md`, and review. A second meaning has been introduced twice. |
| A test named in the evidence table must exist. | `scripts/preflight.sh` runs `scripts/evidence-status.py`, which fails on a name with no test. |
| A session that can write holds one `OpenLibrary`, never a loose id beside a handle. | The type. `open_library` is the only constructor. |
| The gate that guards a commit also guards the push. Never copy its commands into a workflow. | `.github/workflows/preflight.yml` runs `scripts/preflight.sh` on every pull request and on every push to `main`, so a command added to the script runs in CI with no other edit. |

## Not yet enforced

These are judgment calls. They are here so the next agent knows the intent.

- **One idea per sentence in prose, no em dashes.** No linter runs over the
  markdown yet.
- **A commit subject in sentence case, imperative mood.** No commit-msg hook
  yet. A hook would need `core.hooksPath` set, and that is a per-clone step.
  `.github/workflows/preflight.yml` runs the tests and the rules on every pull
  request, but it does not read commit subjects, so this one is still review only.
- **No `unwrap()` outside tests.** `clippy` does not check this by default.
  Adding `clippy::unwrap_used` to the preflight would enforce it, but the
  existing tests would need auditing first.
- **A slow step never goes on the startup path.** `tmutil` took eleven seconds
  per call in `Store::open`, against eleven milliseconds for the extended
  attribute that does the same job. No check catches this, so measure a call
  before putting it in a path the user waits for.
