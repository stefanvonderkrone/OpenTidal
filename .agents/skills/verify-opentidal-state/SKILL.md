---
name: verify-opentidal-state
description: Formal verification of the OpenTidal state-machine invariant in src/state.rs (accent in range and navigation stack non-empty). Use when a change touches src/state.rs or the state handling in src/main.rs.
---

# Verify the OpenTidal state invariant

**The property.** Every reachable `State` satisfies `accent < ACCENT_LEN` (6) and has a non-empty
navigation stack.

**The harm it prevents.** `handle_key` advances the accent with `accent + 1`. If `accent` could
reach 6 or more, that addition keeps growing toward `usize` overflow, a panic in debug builds and
a wrap in release. An empty stack means the app has no current screen; `current` falls back to
Home, but the invariant is that the fallback is never taken.

**The tool.** Not a model checker and not a hand model. Two Rust tests in `src/state.rs` run on the
real code that `src/main.rs` calls, so they cannot drift from the code the way a separate spec
could.

- `every_reachable_state_satisfies_invariant` explores every reachable state up to stack depth 3
  and asserts the invariant at each. The accent dimension is explored in full, all six accents and
  both playing states are reached at depth one, so the accent bound holds at every depth, not only
  up to the bound. Depth 3 is the shell's real limit, section to detail to detail.
- `back_never_empties_the_stack` checks the back transition at the root and at detail depth.

## Run it

```sh
cargo test
```

at the repo root. One command, well under a second after the first compile, and it runs only
`opentidal`.

## Read a failure

A failure is a panic of one of two shapes:

```
reachable state breaks the invariant: State { stack: [Home], playing: true, accent: 6 }
```

means the app can reach a state where `accent + 1` is no longer safe.

```
reachable state breaks the invariant: State { stack: [], playing: true, accent: 0 }
```

means the app can reach a state with no current screen.

Tell a real bug from a modeling error:

- **Real bug.** The counterexample state is genuinely reachable. Removing the `% ACCENT_LEN` in
  `handle_key` produces the accent counterexample after six `a` presses. Making `back` pop
  unconditionally produces the empty-stack counterexample. Fix the code, not the test.
- **Modeling error.** The test's `inits()` or `successors()` no longer matches `src/main.rs`. A new
  route, event, or scene was added to the app but not to the test, or `ACCENT_LEN` drifted from
  `rune::tok::ACCENT_NAMES.len()`. Update the test to match the code, then re-prove.
- **Vacuity guard.** The test asserts it reached all six accents and both playing states, so a pass
  is never vacuous.

Both failure shapes were proven able to fail: removing `% ACCENT_LEN` and making `back` pop
unconditionally each fail their test, then pass again when reverted.

## Coverage record

Verified at commit `3d70447` (`Build the multi-screen shell`).

Covered code:

- `src/state.rs` — `Route`, `Group`, `State`, `invariant`, `current`, `open_section`,
  `open_detail`, `back`, `tap_play`, `handle_key`, `SCENES`, `inits`, and the `tests` module.
- `src/main.rs` — the call sites that must keep using `state`: the sidebar `on_press`
  (`state::open_section`), the card `on_press` (`state::open_detail`), the top-bar back button
  (`state::back`), the top-bar Search and Settings buttons (`state::open_section`), the play button
  (`state::tap_play`), the `on_key` handler (`state::handle_key`), and
  `const NAV_LEN` via `state::SECTIONS`.

Assumptions and bounds:

- The accent bound is exhaustive over the finite accent/playing domain. The stack check is
  exhaustive up to depth 3; deeper is unrepresentable in the shell because `open_detail` dedups
  consecutive routes and `open_section` resets to depth one, so depth is bounded by the number of
  distinct routes.
- Events outside the set in `successors()` are not covered until they are added there.
- This checks the app's own state, not the vendored `rune` framework.

## Update it

When you change `src/state.rs` or the state handling in `src/main.rs`:

1. Run `cargo test`.
2. If it fails and the change was intended, update `invariant`, `inits`, or `successors` to
   describe the new code, re-run until it passes, and prove the new test still fails on a
   deliberate break.
3. Only after you have confirmed the verification still describes the code, move the sync commit
   above to your new commit. Do not move it forward on a passing run you did not actually
   re-verify against the changed code.

Bring this back in line after changes that skipped the re-check with
`@maintain-formal-verification-skill`.
