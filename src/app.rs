//! The rune app state: the proven [`State`] core plus the ephemeral
//! [`TextEdit`] that backs the search field. Keeping the search field's edit
//! buffer here, and out of `State`, is what lets `State` stay `PartialEq`/`Eq`/
//! `Hash` and keeps the invariant proof bounded. The proof checks only `core`.

use rune::TextEdit;
use crate::state::State;

pub struct App {
    pub core: State,
    pub search: TextEdit,
}

impl Default for App {
    fn default() -> Self {
        App { core: State::default(), search: TextEdit::new("") }
    }
}
