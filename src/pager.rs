//! Which page of which remote list to read next. Rune-free and free of I/O.
//! The app hands each `PageRequest` to the page worker and lands its
//! `PageResult` here, so every scheduling and renewal rule lives in one place.
#![cfg_attr(not(test), expect(dead_code, reason = "the page worker is the first caller"))]

use std::collections::{BTreeMap, BTreeSet};

use crate::library::{Merge, Reach, RemoteList};
use crate::tidal::{Cursor, Page, ServiceError};

/// A pass reads at most this many pages, so a list that never ends cannot hold
/// the worker forever.
const MAX_PAGES: u16 = 400;

/// The lists a sync reads to the end. A playlist's tracks are read only when
/// the user opens it.
const MIRRORED: [RemoteList; 4] = [RemoteList::Liked, RemoteList::Albums, RemoteList::Artists, RemoteList::Playlists];

/// Minted from a counter the app never resets, so a result from a replaced
/// pass, or from another account's passes, finds no pass and lands nothing.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct PassId(u64);

/// How far a pass must read. Grows only.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Want {
    Rows(usize),
    All,
}

impl Want {
    fn grows_to(self, to: Want) -> bool {
        match (self, to) {
            (Want::All, _) => false,
            (Want::Rows(_), Want::All) => true,
            (Want::Rows(from), Want::Rows(to)) => to > from,
        }
    }
}

/// One read of one list, from its first page.
struct Pass {
    id: PassId,
    want: Want,
    /// Every member id read so far, page one first, without duplicates.
    confirmed: Vec<String>,
    state: State,
    /// Set by a rejection, cleared by a page that lands. A second rejection
    /// while it is set means the renewed token is dead too.
    renewed: bool,
}

enum State {
    Next {
        /// `None` until page one lands.
        cursor: Option<Cursor>,
        phase: Phase,
        /// Every cursor this pass read a page from.
        followed: BTreeSet<Cursor>,
        pages: u16,
    },
    End,
}

enum Phase {
    Ready,
    Fetching,
    /// Waits until the session's access token differs from this one.
    Renewing { rejected: String },
    /// Keeps the cursor, so the next larger demand retries it.
    Stalled(ServiceError),
}

/// The passes of one account's open library.
#[derive(Default)]
pub struct Passes {
    by: BTreeMap<RemoteList, Pass>,
    /// The list of the top route at the last `enter`.
    current: Option<RemoteList>,
}

/// One page for the worker to read with the session's access token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageRequest {
    pub pass: PassId,
    pub of: RemoteList,
    pub after: Option<Cursor>,
}

/// A finished page, tagged so `land` can find its pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageResult {
    pub pass: PassId,
    pub result: Result<Page, ServiceError>,
    /// The access token the request used, so a rejection names it.
    pub token: String,
}

/// What the app does with a landed page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Landed {
    /// Apply with `Library::merge`.
    Merge(Merge),
    /// The token was rejected. Start one profile fetch to renew it.
    Renew,
    /// The grant is gone. End the session.
    DeadGrant(ServiceError),
    /// Nothing to apply. A stall shows through `status`.
    Kept,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PassStatus {
    Idle,
    Loading,
    Complete,
    Stalled(String),
}

/// Why a pass must not read the page at `cursor`, if it must not.
fn stop(cursor: Option<&Cursor>, followed: &BTreeSet<Cursor>, pages: u16) -> Option<ServiceError> {
    if pages >= MAX_PAGES {
        return Some(ServiceError::Paging("page cap".to_string()));
    }
    if cursor.is_some_and(|c| followed.contains(c)) {
        return Some(ServiceError::Paging("repeated cursor".to_string()));
    }
    None
}

impl Pass {
    fn new(next_id: &mut u64, want: Want) -> Pass {
        let id = PassId(*next_id);
        *next_id += 1;
        let state = State::Next { cursor: None, phase: Phase::Ready, followed: BTreeSet::new(), pages: 0 };
        Pass { id, want, confirmed: Vec::new(), state, renewed: false }
    }

    /// Still reading, so a new demand joins it instead of replacing it.
    fn running(&self) -> bool {
        matches!(&self.state, State::Next { phase, .. } if !matches!(phase, Phase::Stalled(_)))
    }

    fn fetching(&self) -> bool {
        matches!(&self.state, State::Next { phase: Phase::Fetching, .. })
    }

    fn rejected(&self, token: &str) -> bool {
        matches!(&self.state, State::Next { phase: Phase::Renewing { rejected }, .. } if rejected == token)
    }

    /// Page one always revalidates the cache. Later pages wait for demand.
    fn wants_more(&self) -> bool {
        match (&self.state, self.want) {
            (State::End, _) => false,
            (State::Next { .. }, Want::All) => true,
            (State::Next { pages, .. }, Want::Rows(rows)) => *pages == 0 || self.confirmed.len() < rows,
        }
    }

    /// `Passes::next_request` already refused a token this pass saw rejected.
    fn start(&mut self, of: &RemoteList) -> Option<PageRequest> {
        let wants_more = self.wants_more();
        let State::Next { cursor, phase, followed, pages } = &mut self.state else {
            return None;
        };
        if !matches!(phase, Phase::Ready | Phase::Renewing { .. }) || !wants_more {
            return None;
        }
        if let Some(why) = stop(cursor.as_ref(), followed, *pages) {
            *phase = Phase::Stalled(why);
            return None;
        }
        *phase = Phase::Fetching;
        Some(PageRequest { pass: self.id, of: of.clone(), after: cursor.clone() })
    }

    fn land(&mut self, of: &RemoteList, result: Result<Page, ServiceError>, token: String) -> Landed {
        let State::Next { cursor, phase, followed, pages } = &mut self.state else {
            return Landed::Kept;
        };
        if !matches!(phase, Phase::Fetching) {
            return Landed::Kept;
        }
        let page = match result {
            Ok(page) => page,
            Err(ServiceError::Unauthorized) if self.renewed => {
                *phase = Phase::Stalled(ServiceError::Unauthorized);
                return Landed::DeadGrant(ServiceError::Unauthorized);
            }
            Err(ServiceError::Unauthorized) => {
                self.renewed = true;
                *phase = Phase::Renewing { rejected: token };
                return Landed::Renew;
            }
            Err(error @ ServiceError::InvalidGrant(_)) => {
                *phase = Phase::Stalled(error.clone());
                return Landed::DeadGrant(error);
            }
            Err(error) => {
                *phase = Phase::Stalled(error);
                return Landed::Kept;
            }
        };
        self.renewed = false;
        for member in page.members {
            if !self.confirmed.contains(&member) {
                self.confirmed.push(member);
            }
        }
        if let Some(read) = cursor.take() {
            followed.insert(read);
        }
        *pages += 1;
        let reach = match page.next {
            Some(next) => {
                *phase = stop(Some(&next), followed, *pages).map_or(Phase::Ready, Phase::Stalled);
                *cursor = Some(next);
                Reach::Partial
            }
            None => {
                self.state = State::End;
                Reach::Complete
            }
        };
        Landed::Merge(Merge { list: of.clone(), rows: page.rows, confirmed: self.confirmed.clone(), reach })
    }
}

impl Passes {
    /// Keeps a running pass for `of` and raises its demand to `want`. Replaces
    /// a finished or stalled pass, or adds one, reading from page one.
    fn start_or_join(&mut self, of: RemoteList, want: Want, next_id: &mut u64) {
        match self.by.get_mut(&of) {
            Some(pass) if pass.running() => {
                if pass.want.grows_to(want) {
                    pass.want = want;
                }
            }
            _ => {
                self.by.insert(of, Pass::new(next_id, want));
            }
        }
    }

    /// The top route now shows `of`. Only a change of list starts a pass, so
    /// calling this every frame is harmless.
    pub fn enter(&mut self, of: Option<RemoteList>, next_id: &mut u64) {
        if self.current == of {
            return;
        }
        self.current = of.clone();
        if let Some(of) = of {
            self.start_or_join(of, Want::Rows(0), next_id);
        }
    }

    /// The view shows the first `rows` members of `of`. A stalled pass whose
    /// demand grows tries again.
    pub fn want(&mut self, of: &RemoteList, rows: usize) {
        let Some(pass) = self.by.get_mut(of) else {
            return;
        };
        if !pass.want.grows_to(Want::Rows(rows)) {
            return;
        }
        pass.want = Want::Rows(rows);
        if let State::Next { phase, .. } = &mut pass.state
            && matches!(phase, Phase::Stalled(_))
        {
            *phase = Phase::Ready;
        }
    }

    /// A sync. Every mirrored list reads to its last page.
    pub fn read_to_end(&mut self, next_id: &mut u64) {
        for of in MIRRORED {
            self.start_or_join(of, Want::All, next_id);
        }
    }

    /// The next page to read, if any, marked as fetching. One page at a time,
    /// the current list first, then the others in `RemoteList` order. No pass
    /// reads with a token another pass has seen rejected.
    pub fn next_request(&mut self, token: &str) -> Option<PageRequest> {
        if self.by.values().any(|p| p.fetching() || p.rejected(token)) {
            return None;
        }
        let current = self.current.clone();
        let rest: Vec<RemoteList> = self.by.keys().filter(|of| Some(*of) != current.as_ref()).cloned().collect();
        current.into_iter().chain(rest).find_map(|of| self.by.get_mut(&of)?.start(&of))
    }

    /// Land one result on the pass that asked for it.
    pub fn land(&mut self, landed: PageResult) -> Landed {
        match self.by.iter_mut().find(|(_, pass)| pass.id == landed.pass) {
            Some((of, pass)) => pass.land(of, landed.result, landed.token),
            None => Landed::Kept,
        }
    }

    pub fn status(&self, of: &RemoteList) -> PassStatus {
        let Some(pass) = self.by.get(of) else {
            return PassStatus::Idle;
        };
        match &pass.state {
            State::End => PassStatus::Complete,
            State::Next { phase: Phase::Stalled(error), .. } => PassStatus::Stalled(format!("{error:?}")),
            State::Next { phase: Phase::Fetching | Phase::Renewing { .. }, .. } => PassStatus::Loading,
            State::Next { phase: Phase::Ready, .. } if pass.wants_more() => PassStatus::Loading,
            State::Next { phase: Phase::Ready, .. } => PassStatus::Idle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::{Catalogue, CatalogueRows};

    fn page(members: &[&str], next: Option<&str>) -> Result<Page, ServiceError> {
        Ok(Page {
            rows: CatalogueRows::default(),
            members: members.iter().map(|m| m.to_string()).collect(),
            next: next.map(Cursor::fake),
        })
    }

    fn answer(request: &PageRequest, result: Result<Page, ServiceError>, token: &str) -> PageResult {
        PageResult { pass: request.pass, result, token: token.to_string() }
    }

    /// Takes the next request and lands `result` on it.
    fn serve(passes: &mut Passes, result: Result<Page, ServiceError>) -> (PageRequest, Landed) {
        let request = passes.next_request("t").expect("no page was requested");
        let landed = passes.land(answer(&request, result, "t"));
        (request, landed)
    }

    fn reach(landed: &Landed) -> Option<Reach> {
        match landed {
            Landed::Merge(m) => Some(m.reach),
            _ => None,
        }
    }

    #[test]
    fn entering_the_same_list_twice_keeps_one_pass_and_another_list_starts_its_own() {
        let mut passes = Passes::default();
        let mut ids = 0;
        passes.enter(Some(RemoteList::Liked), &mut ids);
        passes.want(&RemoteList::Liked, 5);
        let first = passes.next_request("t").unwrap();
        passes.enter(Some(RemoteList::Liked), &mut ids);
        assert_eq!(passes.next_request("t"), None, "a second enter started a second read");
        passes.enter(Some(RemoteList::Albums), &mut ids);
        passes.enter(Some(RemoteList::Liked), &mut ids);
        let landed = passes.land(answer(&first, page(&["a"], Some("1")), "t"));
        assert_eq!(reach(&landed), Some(Reach::Partial), "entering the list again replaced the running pass");

        passes.enter(Some(RemoteList::Albums), &mut ids);
        let (albums, _) = serve(&mut passes, page(&[], None));
        assert_eq!((albums.of, albums.after), (RemoteList::Albums, None));
        assert_ne!(albums.pass, first.pass);
        assert_eq!(passes.status(&RemoteList::Liked), PassStatus::Loading, "entering another list dropped the pass");
        let resumed = passes.next_request("t").unwrap();
        assert_eq!((resumed.pass, resumed.after), (first.pass, Some(Cursor::fake("1"))));
    }

    #[test]
    fn entering_a_finished_list_again_reads_nothing() {
        let mut passes = Passes::default();
        let mut ids = 0;
        passes.enter(Some(RemoteList::Liked), &mut ids);
        serve(&mut passes, page(&["a"], None));
        passes.enter(Some(RemoteList::Liked), &mut ids);
        assert_eq!(passes.next_request("t"), None, "staying on a list read it again");
        assert_eq!(passes.status(&RemoteList::Liked), PassStatus::Complete);
    }

    #[test]
    fn returning_to_a_finished_list_reads_it_again_from_page_one() {
        let mut passes = Passes::default();
        let mut ids = 0;
        passes.enter(Some(RemoteList::Liked), &mut ids);
        let (first, _) = serve(&mut passes, page(&["a"], None));
        assert_eq!(passes.status(&RemoteList::Liked), PassStatus::Complete);
        passes.enter(Some(RemoteList::Albums), &mut ids);
        serve(&mut passes, page(&[], None));
        passes.enter(Some(RemoteList::Liked), &mut ids);
        let again = passes.next_request("t").unwrap();
        assert_eq!((again.of.clone(), again.after.clone()), (RemoteList::Liked, None));
        assert_ne!(again.pass, first.pass);
    }

    #[test]
    fn at_most_one_page_is_in_flight() {
        let mut passes = Passes::default();
        let mut ids = 0;
        passes.enter(Some(RemoteList::Liked), &mut ids);
        let request = passes.next_request("t").unwrap();
        passes.enter(Some(RemoteList::Albums), &mut ids);
        assert_eq!(passes.next_request("t"), None, "a second page went out while one was in flight");
        assert_eq!(passes.status(&RemoteList::Albums), PassStatus::Loading);
        passes.land(answer(&request, page(&["a"], None), "t"));
        assert_eq!(passes.next_request("t").map(|r| r.of), Some(RemoteList::Albums));
    }

    #[test]
    fn demand_only_grows() {
        let mut passes = Passes::default();
        let mut ids = 0;
        passes.enter(Some(RemoteList::Liked), &mut ids);
        passes.want(&RemoteList::Liked, 2);
        let request = passes.next_request("t").unwrap();
        assert_eq!(passes.status(&RemoteList::Liked), PassStatus::Loading);
        passes.land(answer(&request, page(&["a"], Some("1")), "t"));

        passes.want(&RemoteList::Liked, 1);
        let (second, _) = serve(&mut passes, page(&["b"], Some("2")));
        assert_eq!(second.after, Some(Cursor::fake("1")), "a smaller demand shrank the want");
        assert_eq!(passes.next_request("t"), None, "the pass read past its demand");
        assert_eq!(passes.status(&RemoteList::Liked), PassStatus::Idle);

        passes.want(&RemoteList::Liked, 3);
        assert_eq!(passes.next_request("t").map(|r| r.after), Some(Some(Cursor::fake("2"))));
    }

    #[test]
    fn the_current_list_goes_first() {
        let mut passes = Passes::default();
        let mut ids = 0;
        passes.enter(Some(RemoteList::Liked), &mut ids);
        passes.enter(Some(RemoteList::Albums), &mut ids);
        let (first, _) = serve(&mut passes, page(&[], None));
        let (second, _) = serve(&mut passes, page(&[], None));
        assert_eq!((first.of, second.of), (RemoteList::Albums, RemoteList::Liked));
    }

    #[test]
    fn a_final_page_is_complete_and_an_earlier_page_is_partial() {
        let mut passes = Passes::default();
        let mut ids = 0;
        passes.enter(Some(RemoteList::Liked), &mut ids);
        passes.want(&RemoteList::Liked, 10);
        let (_, first) = serve(&mut passes, page(&["a"], Some("1")));
        let (_, last) = serve(&mut passes, page(&["b", "a"], None));
        let confirmed = |l: &Landed| match l {
            Landed::Merge(m) => m.confirmed.clone(),
            _ => Vec::new(),
        };
        assert_eq!(reach(&first), Some(Reach::Partial));
        assert_eq!(confirmed(&first), ["a"]);
        assert_eq!(reach(&last), Some(Reach::Complete));
        assert_eq!(confirmed(&last), ["a", "b"], "the confirmed list is not page one first without duplicates");
        assert_eq!(passes.status(&RemoteList::Liked), PassStatus::Complete);
        assert_eq!(passes.next_request("t"), None, "a finished pass asked for another page");
    }

    #[test]
    fn a_repeated_cursor_stalls_the_pass_for_good() {
        let mut passes = Passes::default();
        let mut ids = 0;
        passes.enter(Some(RemoteList::Liked), &mut ids);
        passes.want(&RemoteList::Liked, 10);
        serve(&mut passes, page(&["a"], Some("1")));
        let (_, landed) = serve(&mut passes, page(&["b"], Some("1")));
        assert_eq!(reach(&landed), Some(Reach::Partial), "the page before the loop was not merged");
        assert!(matches!(passes.status(&RemoteList::Liked), PassStatus::Stalled(_)));
        assert_eq!(passes.next_request("t"), None);
        passes.want(&RemoteList::Liked, 20);
        assert_eq!(passes.next_request("t"), None, "more demand read the repeated cursor");
    }

    #[test]
    fn an_error_stalls_the_pass_until_demand_grows() {
        let mut passes = Passes::default();
        let mut ids = 0;
        passes.enter(Some(RemoteList::Liked), &mut ids);
        passes.want(&RemoteList::Liked, 10);
        serve(&mut passes, page(&["a"], Some("1")));
        let (_, landed) = serve(&mut passes, Err(ServiceError::Http("500".into())));
        assert_eq!(landed, Landed::Kept);
        assert!(matches!(passes.status(&RemoteList::Liked), PassStatus::Stalled(_)));
        assert_eq!(passes.next_request("t"), None, "a stalled pass retried on its own");
        passes.want(&RemoteList::Liked, 10);
        assert_eq!(passes.next_request("t"), None, "the same demand retried a stalled pass");
        passes.want(&RemoteList::Liked, 11);
        let retry = passes.next_request("t").expect("more demand did not retry the stalled pass");
        assert_eq!(retry.after, Some(Cursor::fake("1")), "the retry lost the cursor");
    }

    #[test]
    fn the_page_cap_stalls_the_pass() {
        let mut passes = Passes::default();
        let mut ids = 0;
        passes.enter(Some(RemoteList::Liked), &mut ids);
        passes.want(&RemoteList::Liked, usize::MAX);
        for n in 0..MAX_PAGES {
            let (_, landed) = serve(&mut passes, page(&[&n.to_string()], Some(&(n + 1).to_string())));
            assert_eq!(reach(&landed), Some(Reach::Partial), "page {n} did not merge");
        }
        assert_eq!(passes.next_request("t"), None, "the pass read past the page cap");
        assert!(matches!(passes.status(&RemoteList::Liked), PassStatus::Stalled(_)));
    }

    #[test]
    fn a_result_for_an_unknown_pass_lands_nothing() {
        let mut passes = Passes::default();
        let mut ids = 0;
        passes.enter(Some(RemoteList::Liked), &mut ids);
        let request = passes.next_request("t").unwrap();
        let stranger = PageResult { pass: PassId(request.pass.0 + 1), result: page(&["x"], None), token: "t".into() };
        assert_eq!(passes.land(stranger), Landed::Kept);
        assert_eq!(passes.status(&RemoteList::Liked), PassStatus::Loading, "a stranger's result finished the pass");
        assert_eq!(reach(&passes.land(answer(&request, page(&["a"], None), "t"))), Some(Reach::Complete));
    }

    #[test]
    fn a_rejected_token_renews_once_and_a_second_rejection_is_a_dead_grant() {
        let mut passes = Passes::default();
        let mut ids = 0;
        passes.enter(Some(RemoteList::Liked), &mut ids);
        let request = passes.next_request("old").unwrap();
        assert_eq!(passes.land(answer(&request, Err(ServiceError::Unauthorized), "old")), Landed::Renew);
        assert_eq!(passes.next_request("old"), None, "the pass read again with the rejected token");
        let retry = passes.next_request("new").expect("a new token did not resume the pass");
        assert_eq!(retry.after, request.after);
        assert_eq!(
            passes.land(answer(&retry, Err(ServiceError::Unauthorized), "new")),
            Landed::DeadGrant(ServiceError::Unauthorized),
            "a renewed token that was rejected again did not end the grant"
        );
    }

    #[test]
    fn a_page_that_lands_after_a_renewal_lets_a_later_expiry_renew_again() {
        let mut passes = Passes::default();
        let mut ids = 0;
        passes.enter(Some(RemoteList::Liked), &mut ids);
        passes.want(&RemoteList::Liked, 10);
        let request = passes.next_request("a").unwrap();
        assert_eq!(passes.land(answer(&request, Err(ServiceError::Unauthorized), "a")), Landed::Renew);
        let retry = passes.next_request("b").unwrap();
        passes.land(answer(&retry, page(&["x"], Some("1")), "b"));
        let later = passes.next_request("b").unwrap();
        assert_eq!(passes.land(answer(&later, Err(ServiceError::Unauthorized), "b")), Landed::Renew);
    }

    #[test]
    fn a_revoked_grant_is_dead_at_once() {
        let mut passes = Passes::default();
        let mut ids = 0;
        passes.enter(Some(RemoteList::Liked), &mut ids);
        let revoked = ServiceError::InvalidGrant("revoked".into());
        let (_, landed) = serve(&mut passes, Err(revoked.clone()));
        assert_eq!(landed, Landed::DeadGrant(revoked));
    }

    #[test]
    fn a_sync_reads_every_mirrored_list_to_the_end_and_no_playlist() {
        let cat = Catalogue::seed();
        let playlist = RemoteList::Playlist(cat.playlist_id("seed-playlist-0").unwrap());
        let mut passes = Passes::default();
        let mut ids = 0;
        passes.enter(Some(playlist.clone()), &mut ids);
        serve(&mut passes, page(&[], None));
        passes.read_to_end(&mut ids);
        let mut read = Vec::new();
        while let Some(request) = passes.next_request("t") {
            let next = if request.after.is_none() { Some("1") } else { None };
            passes.land(answer(&request, page(&[], next), "t"));
            read.push((request.of, request.after.is_some()));
        }
        let expected: Vec<(RemoteList, bool)> =
            MIRRORED.iter().flat_map(|of| [(of.clone(), false), (of.clone(), true)]).collect();
        assert_eq!(read, expected, "a sync did not read each mirrored list to its last page");
        for of in MIRRORED {
            assert_eq!(passes.status(&of), PassStatus::Complete);
        }
    }
}
