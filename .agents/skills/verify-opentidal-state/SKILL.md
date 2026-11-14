---
name: verify-opentidal-state
description: Formal verification of the OpenTidal state-machine invariant in src/state.rs (accent and nav always in range). Use when a change touches src/state.rs or the state handling in src/main.rs.
---

# Verify the OpenTidal state invariant

**The property.** Every reachable `State` satisfies `accent < ACCENT_LEN` (6) and `nav < NAV_LEN`
(6).

**The harm it prevents.** `handle_key` advances the accent with `accent + 1`. If `accent` could
reach 6 or more, that addition keeps growing toward `usize` overflow — a panic in debug builds, a
wrap in release. A `nav` at or past `NAV_LEN` would mean no section is selected (a silent wrong
screen). Both are out-of-range states the invariant forbids.

**The tool.** Not a model checker and not a hand model: one Rust test
(`state::tests::every_reachable_state_satisfies_invariant` in `src/state.rs`) exhaustively explores
the reachable state space from every scene initializer, applying every event, and asserts the
invariant at each state. The domain is finite — `NAV_LEN * ACCENT_LEN * 2` = 72 states — so
enumerating it is a complete proof, not a sample. It runs on the real code that `src/main.rs`
calls, so it cannot drift from the code the way a separate spec could.

## Run it

```sh
cargo test
```

at the repo root. One command. It runs only `opentidal` (the `rune` workspace is excluded), and it
finishes in well under a second after the first compile. The test is the only check; there is no
separate tool to install.

## Read a failure

A failure is a panic of this shape:

```
reachable state breaks the invariant: State { nav: 0, playing: true, accent: 6 }
```

The state names the counterexample directly: a state the app can reach where `accent` or `nav` is
out of range. Read it as "the app can get into a state where `accent + 1` is no longer safe".

Tell a real bug from a modeling error:

- **Real bug.** The counterexample state is genuinely reachable in the code — for example someone
  removed the `% ACCENT_LEN` in `handle_key` (accent grows without bound), or a scene in `SCENES`
  now sets `accent` or `nav` out of range. Fix the code, not the test.
- **Modeling error.** The test's `inits()` or `successors()` no longer match `src/main.rs` — a new
  event or scene was added to the app but not to the test, or `NAV_LEN` / `ACCENT_LEN` drifted
  from the real counts. In that case the test is asserting against a stale model; update the test
  to match the code, then re-prove.
- **Vacuity guard.** The final `seen.len() == NAV_LEN * ACCENT_LEN * 2` assertion fails if the
  exploration did not cover the whole domain (a missing event or a wrong bound), so a pass is never
  vacuous.

This test was proven able to fail: removing `% ACCENT_LEN` from `handle_key` makes it fail with
exactly the counterexample above, at `accent: 6`.

## Coverage record

Verified at commit `da9648f` (`Extract app state machine and prove its invariant exhaustively`).

Covered code:

- `src/state.rs` — `State`, `invariant`, `tap_nav`, `tap_play`, `handle_key`, `SCENES`, `inits`,
  and the `tests` module itself.
- `src/main.rs` — the call sites that must keep using `state`: the nav `on_press`
  (`state::tap_nav`), the play `on_press` (`state::tap_play`), the `on_key` handler
  (`state::handle_key`), `scenes: &state::SCENES`, the `ACCENT_LEN` compile-time assert, and
  `const NAV: [&str; state::NAV_LEN]`.

Assumptions and bounds:

- The invariant is exhaustive only over the finite domain. If a change makes the state unbounded
  (for example a transition that adds instead of wrapping), the test fails at the first
  out-of-range state rather than looping forever, because the invariant assertion fires on the
  first offending state.
- Events outside the four kinds in `successors()` — a future key, a gesture, a menu — are not
  covered until they are added there.
- This checks the app's own state, not the vendored `rune` framework.

## Update it

When you change `src/state.rs` or the state handling in `src/main.rs`:

1. Run `cargo test`.
2. If it fails and the change was intended, update `invariant`, `inits`, or `successors` to
   describe the new code, re-run until it passes, and prove the new test is still able to fail on a
   deliberate break.
3. Only after you have confirmed the verification still describes the code, move the sync commit
   above to your new commit. Do not move it forward on a passing run you did not actually
   re-verify against the changed code.

Bring this back in line after changes that skipped the re-check with
`@maintain-formal-verification-skill`.
