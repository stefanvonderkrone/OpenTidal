---
name: verify-opentidal-state
description: Formal verification of the OpenTidal invariants in src/state.rs and src/library.rs (accent in range, navigation stack non-empty, and every saved item, playlist and route references a valid id). Use when a change touches src/state.rs, src/library.rs, or the state handling in src/main.rs.
---

# Verify the OpenTidal invariants

**The property.** Every reachable `State` satisfies all of:

- `accent < ACCENT_LEN` (6), and a non-empty navigation stack (`src/state.rs`).
- The now-playing track is a valid catalogue id.
- No route points at a deleted user playlist.
- `Library::check` holds: every bitset bit, history entry, playlist track and
  collection playlist is a valid id, every list is within its cap, and no
  list has a duplicate (`src/library.rs`).

**The harm it prevents.** `handle_key` advances the accent with `accent + 1`; an
accent at or past 6 grows toward `usize` overflow. An empty stack leaves the
app with no current screen. A dangling playlist route or an out-of-range id
would make the view index a playlist or track that does not exist.

**The tool.** Not a model checker and not a hand model. Rust tests run on the
real code that `src/main.rs` calls, so they cannot drift from the code the way
a separate spec could. The proof is split where the state splits:

- `src/state.rs` — `every_reachable_state_satisfies_invariant` explores every
  reachable nav state up to depth 3 and asserts the combined invariant. It
  exercises the two library ops that couple the library to the stack (create
  and delete a playlist), because those are what the dangling-route invariant
  needs. `deleting_playlist_cleans_the_stack` and
  `opening_missing_playlist_is_refused` pin the two guards. `random_walk_keeps_invariant`
  runs 20,000 steps over the full operation surface.
- `src/library.rs` — `every_reachable_library_satisfies_check` explores every
  reachable `Library` state and asserts `check` at each. `check_detects_violations`
  proves the checker actually rejects a bad id, so the proof is not vacuous.

## Run it

```sh
cargo test
```

at the repo root. One command, well under a minute after the first compile, and
it runs only `opentidal`.

## Read a failure

A failure is a panic with one of three shapes:

```
reachable state breaks the invariant: State { stack: [Home], playing: true, accent: 6 }
```

means the app can reach a state where `accent + 1` is no longer safe.

```
reachable state breaks the invariant: State { stack: [], playing: true, accent: 0 }
```

means the app can reach a state with no current screen.

```
library broke the invariant: Library { .. }
```

means some library reference went out of range, past a cap, or duplicated.

Tell a real bug from a modeling error:

- **Real bug.** The counterexample state is genuinely reachable. Removing the
  `% ACCENT_LEN` in `handle_key` produces the accent counterexample. Making
  `back` pop unconditionally produces the empty-stack counterexample. Removing
  the `retain` in `delete_playlist` produces the dangling-route counterexample.
  Fix the code, not the test.
- **Modeling error.** A test's `successors()` no longer matches the code. A new
  route, event, or operation was added to the app but not to the test, or
  `ACCENT_LEN` drifted from `rune::tok::ACCENT_NAMES.len()`. Update the test to
  match the code, then re-prove.

The failure shapes were proven able to fail: each deliberate break fails its
test, then passes again when reverted.

## Coverage record

Verified at commit `1c6c4f1` (`Add feature journeys and update docs`).

Covered code:

- `src/state.rs` — `Route`, `Group`, `State`, `invariant`, `current`,
  `open_section`, `open_detail`, `back`, `tap_play`, `toggle_sidebar`,
  `set_popular_tab`, `select_track`, `delete_playlist`, `handle_key`, and the
  `tests` module.
- `src/library.rs` — `Library`, `Playlist`, `Collection`, every operation, and
  `check`.
- `src/main.rs` — the call sites that must keep using `state`: the sidebar
  `on_press` (`state::open_section`), the card `on_press` (`state::open_detail`),
  the top-bar back button (`state::back`), the top-bar Search and Settings
  buttons (`state::open_section`), the play button (`state::tap_play`), the
  `on_key` handler (`state::handle_key`), and `const NAV_LEN` via `state::SECTIONS`.

Assumptions and bounds:

- Library bounds (`MAX_PLAYLISTS`, `HISTORY_CAP`, `PLAYLIST_TRACK_CAP`,
  `MAX_COLLECTIONS`, `COLLECTION_PLAYLIST_CAP`) shrink under `cfg(test)` so the
  state space stays finite. The nav proof is exhaustive up to depth 3; deeper is
  unrepresentable in the shell because `open_detail` dedups and `open_section`
  resets to depth one.
- The exhaustive library proof uses a fixed id set and fixed names; the random
  walk covers the full-size shapes.
- Events outside the set in `successors()` are not covered until they are added
  there.
- This checks the app's own state, not the vendored `rune` framework.

## Update it

When you change `src/state.rs`, `src/library.rs`, or the state handling in
`src/main.rs`:

1. Run `cargo test`.
2. If it fails and the change was intended, update `invariant`, `check`, or a
   `successors` function to describe the new code, re-run until it passes, and
   prove the new test still fails on a deliberate break.
3. Only after you have confirmed the verification still describes the code, move
   the sync commit above to your new commit. Do not move it forward on a passing
   run you did not actually re-verify against the changed code.

Bring this back in line after changes that skipped the re-check with
`@maintain-formal-verification-skill`.
