//! Counts share the protocol selector's logical identity and authorization rules.
use crate::{Store, StoreError};
use domain::{LibraryId, ProtocolItemQuery, ProtocolItemType, ProtocolParent, UserId};
use std::collections::HashSet;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProtocolCounts {
    pub movies: u64,
    pub series: u64,
    pub episodes: u64,
    pub items: u64,
}
impl Store {
    pub fn protocol_counts(&self, user: UserId) -> Result<ProtocolCounts, StoreError> {
        let allowed = self
            .authorized_library_ids(user)?
            .into_iter()
            .map(|id| id.parse())
            .collect::<Result<HashSet<LibraryId>, _>>()?;
        self.with_read(crate::Db::Library, |tx| {
            let total = |kind| {
                super::select_protocol(tx, &allowed, &query(user, vec![kind]))
                    .map(|page| page.total)
            };
            Ok(ProtocolCounts {
                movies: total(ProtocolItemType::Movie)?,
                series: total(ProtocolItemType::Series)?,
                episodes: total(ProtocolItemType::Episode)?,
                items: super::select_protocol(tx, &allowed, &query(user, vec![]))?.total,
            })
        })
    }
}
pub(super) fn query(user: UserId, types: Vec<ProtocolItemType>) -> ProtocolItemQuery {
    ProtocolItemQuery {
        user_id: user,
        parent: ProtocolParent::Root,
        series_id: None,
        season: None,
        person_ids: vec![],
        studio_ids: vec![],
        search_term: None,
        genres: vec![],
        genre_ids: vec![],
        filters: vec![],
        is_favorite: None,
        is_played: None,
        include_types: types,
        recursive: true,
        is_folder: None,
        sort: vec![],
        offset: 0,
        limit: 0,
    }
}
