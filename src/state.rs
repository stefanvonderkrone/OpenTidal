//! OpenTidal's application state and its transitions, extracted from the view so
//! a checker can prove the state-machine invariant without compiling the
//! rune/wgpu view layer. `main.rs` delegates every state change to these
//! functions; nothing here depends on `rune`.

/// Number of sidebar sections. `main.rs` writes the section names as
/// `const NAV: [&str; state::NAV_LEN]`, so the label list cannot drift from this
/// count.
pub const NAV_LEN: usize = 6;

/// Number of accent colours. Matches `rune::tok::ACCENT_NAMES.len()`;
/// `main.rs` asserts that equality at compile time.
pub const ACCENT_LEN: usize = 6;

/// The app's state: the selected section, whether playback is running, and the
/// current accent.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct State {
    pub nav: usize,
    pub playing: bool,
    pub accent: usize,
}

impl Default for State {
    fn default() -> Self {
        State { nav: 0, playing: true, accent: 0 }
    }
}

/// The safety invariant the verification proves: every reachable state has
/// `accent < ACCENT_LEN`, so the `accent + 1` in [`handle_key`] can never
/// overflow `usize`, and `nav < NAV_LEN`, so the selected item is a real
/// section.
pub fn invariant(s: &State) -> bool {
    s.accent < ACCENT_LEN && s.nav < NAV_LEN
}

/// Select a sidebar section by its index.
pub fn tap_nav(s: &mut State, i: usize) {
    s.nav = i;
}

/// Toggle playback from the transport play button.
pub fn tap_play(s: &mut State) {
    s.playing = !s.playing;
}

/// Handle a text key. Returns true when the key changed state. `"a"` advances
/// the accent; `" "` toggles playback; anything else is ignored.
pub fn handle_key(s: &mut State, key: &str) -> bool {
    match key {
        "a" => {
            s.accent = (s.accent + 1) % ACCENT_LEN;
            true
        }
        " " => {
            s.playing = !s.playing;
            true
        }
        _ => false,
    }
}

/// The named scenes `main.rs` passes to `AppSpec`, each applied to
/// `State::default()`.
pub const SCENES: [(&str, fn(&mut State)); 4] = [
    ("default", |_| {}),
    ("accent-blue", |s| s.accent = 0),
    ("accent-pink", |s| s.accent = 2),
    ("paused", |s| s.playing = false),
];

/// Every initial state the app can start from: each scene applied to `default`.
pub fn inits() -> Vec<State> {
    SCENES
        .iter()
        .map(|(_, f)| {
            let mut s = State::default();
            f(&mut s);
            s
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashSet, VecDeque};

    /// The states reachable in one event from `s`: every nav tap, the play tap,
    /// and the two handled keys.
    fn successors(s: State) -> Vec<State> {
        let mut out = Vec::new();
        for i in 0..NAV_LEN {
            let mut t = s;
            tap_nav(&mut t, i);
            out.push(t);
        }
        let mut t = s;
        tap_play(&mut t);
        out.push(t);
        let mut t = s;
        handle_key(&mut t, "a");
        out.push(t);
        let mut t = s;
        handle_key(&mut t, " ");
        out.push(t);
        out
    }

    /// Exhaustively explores every reachable state from every initial state and
    /// asserts the invariant holds at each. The state domain is finite
    /// (`NAV_LEN * ACCENT_LEN * 2` states), so this is a complete proof, not a
    /// sample. The final length check fails if the exploration was not
    /// exhaustive over the whole domain.
    #[test]
    fn every_reachable_state_satisfies_invariant() {
        let mut seen = HashSet::new();
        let mut queue = VecDeque::new();
        for init in inits() {
            assert!(invariant(&init), "initial state breaks the invariant: {init:?}");
            if seen.insert(init) {
                queue.push_back(init);
            }
        }
        while let Some(s) = queue.pop_front() {
            assert!(invariant(&s), "reachable state breaks the invariant: {s:?}");
            for next in successors(s) {
                if seen.insert(next) {
                    queue.push_back(next);
                }
            }
        }
        assert_eq!(
            seen.len(),
            NAV_LEN * ACCENT_LEN * 2,
            "the exploration was not exhaustive over the full state domain"
        );
    }
}
