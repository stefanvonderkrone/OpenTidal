//! The rune app state: the proven [`State`] core plus the ephemeral
//! [`TextEdit`] buffers that back the search field and the rename fields.
//! Keeping the edit buffers here, and out of `State`, is what lets `State`
//! stay `PartialEq`/`Eq`/`Hash` and keeps the invariant proof bounded. The
//! proof checks only `core`.

use rune::TextEdit;
use crate::state::State;

pub struct App {
    pub core: State,
    pub search: TextEdit,
    /// The rename buffer, shared by the playlist and profile rename fields.
    pub rename: TextEdit,
}

impl Default for App {
    fn default() -> Self {
        App {
            core: State::default(),
            search: TextEdit::new(""),
            rename: TextEdit::new(""),
        }
    }
}
