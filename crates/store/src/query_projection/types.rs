use domain::{LibraryId, MediaId};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(crate) struct ProjectionKey {
    pub library_id: LibraryId,
    pub media_id: MediaId,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct CounterMember {
    pub surface: String,
    pub dimension: String,
    pub value: String,
}
impl CounterMember {
    pub fn new(surface: &str, dimension: &str, value: &str) -> Self {
        Self {
            surface: surface.into(),
            dimension: dimension.into(),
            value: value.into(),
        }
    }
}
pub(crate) struct ProjectionBefore {
    pub card_exists: bool,
    pub members: BTreeSet<CounterMember>,
}
#[derive(Default)]
#[allow(dead_code)]
pub(crate) struct ProjectionDelta {
    pub cards: u64,
    pub inserted: u64,
    pub deleted: u64,
    pub counters: u64,
}
pub(super) type Terms = BTreeSet<(String, String)>;
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct Person {
    pub role: String,
    pub name: String,
    pub tmdb: String,
    pub wire: String,
}
