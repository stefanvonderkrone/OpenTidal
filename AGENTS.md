# Agent rules

Every rule here exists because a mistake cost real work, most of them more than
once. Each one names what enforces it, so a rule with no enforcement is a known
repeat.

Run `scripts/preflight.sh` before every commit. It runs the tests, runs clippy
on this crate with warnings as errors, and checks the rules marked below.

Work lands on `main` through a pull request. `main` is protected: the `preflight`
check from GitHub Actions must pass, force pushes and deletions are blocked, the
rules apply to admins too, and an open review thread blocks a merge until someone
resolves it. A direct push is rejected, so push a branch and open a pull request.

The workflow lives on a branch until it merges. Until `.github/workflows/preflight.yml`
reaches `main`, a pull request cut from `main` gets no `preflight` run, so the
required check never reports and that pull request cannot merge.

| Rule | Enforced by |
| --- | --- |
| The token file path comes only from `token_file_path`. It uses `XDG_DATA_HOME` with the `~/.local/share` fallback, mode 0600, atomic replace, a synced directory, no temporary file after a failure, and the backup-exclusion flag on macOS. | `cargo test`, `the_token_file_lives_under_xdg_data_home`, `without_xdg_data_home_the_token_file_falls_back_to_local_share`, `the_token_file_is_private`, `a_file_store_survives_a_restart`, `a_failed_write_keeps_the_previous_token`, and `a_failed_write_leaves_no_temporary_file` |
| Leave `TIDAL_SCOPE` unset in docs and scripts. The list lives in `DEFAULT_SCOPE` only. | `scripts/scope-check.py`, run by `scripts/preflight.sh`. It fails on the literal list or on any assignment of the variable outside `src/auth.rs`, in the working tree, the index or an untracked file |
| A `Library` field is stored by `src/store.rs` and checked by `Library::check`, or it is not part of the library. | `cargo test`. The round-trip test compares whole libraries, so a new field with no column fails it. |
| A session that can write has its library on disk before the app rests or quits. A quit that cannot write reports the lost edits, because nothing can refuse a quit. The deterministic build has no store by design, so it keeps nothing. | `cargo test`, `only_a_dirty_library_asks_for_a_frame`, `a_quit_writes_what_is_outstanding` and `a_quit_that_cannot_write_reports_the_edits_as_lost` |
| A session that can write holds both an account id and an open database, never one without the other. | `open_library` takes the id, so no caller can set one without the other. |
| A signed-in account with no open library says so, and reports its edits as unwritable. | `cargo test`, `an_account_that_cannot_write_says_so` |
| A profile fetch that fails on transport keeps the session, and only a rejected refresh token ends it. | `cargo test`, `a_network_error_keeps_the_session_signed_in` and `a_dead_refresh_token_closes_the_library` |
| A fresh token the API still rejects ends the session. | `cargo test`, `a_fresh_token_that_is_rejected_ends_the_session` |
| A driven scene owns its token store and its database path. | `cargo test`, `the_stored_scene_owns_its_store_and_its_database` |
| The display name lives in `Library`. Do not add a second copy to `Auth` or to a screen. | `cargo test`. The restart test asserts the name comes back from the file. |
| A row with an invalid slot, index or track is dropped without using up a limited row. | `cargo test`, `an_invalid_slot_does_not_hide_a_valid_one`, `an_invalid_collection_index_does_not_hide_a_valid_one` and `an_unknown_history_row_does_not_hide_a_valid_one` |
| One instance owns a library: `Store::open` holds the writer lock. | `cargo test`, `a_second_connection_is_refused_the_library` |
| A store that cannot be read stays closed for writing. | `cargo test`, `a_broken_file_is_never_overwritten` |
| Every `.rs` file stays rune-free above `screens.rs` and `main.rs`. | Review. `src/store.rs`, `src/library.rs`, `src/state.rs`, `src/auth.rs`, and `src/tidal.rs` must not import `rune`. |
| Tests that claim a user-visible outcome must be proven able to fail. Break the code, watch the test fail, revert. | Review. A test whose first version passed for the wrong reason is called out in its commit message. |
| One name per thing. `Store::write` writes the library, **save** means an album, the keychain is `tokens`. | `GLOSSARY.md`, and review. A second meaning has been introduced twice. |
| A test named in the evidence table must exist. In a `live:` row write it as `name`(), or nothing checks it. | `scripts/preflight.sh` runs `scripts/evidence-status.py`, which fails on a name with no test, and on a real test name in a `live:` row without the parentheses. |
| A session that can write holds one `OpenLibrary`, never a loose id beside a handle. | The type. `open_library` is the only constructor. |
| The gate that guards a commit also guards the push. Never copy its commands into a workflow. | `.github/workflows/preflight.yml` runs `scripts/preflight.sh` and `scripts/preflight-linux.sh` on every pull request and on every push to `main`, so a command added to a script runs in CI with no other edit. `main` is protected, so both checks must pass before a merge. |
| A `cfg(target_os = "macos")` function needs a `cfg(not(target_os = "macos"))` twin, or every call site must be gated. | `scripts/preflight-linux.sh` builds and tests the crate on Linux, which is the only thing that notices. It happened once here, and no Mac build can see it. |

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
