//! Authorized playback view pages selected in SQL from sparse playback facts.

use domain::{LibraryId, MediaId, UnitKey, UserId};

use crate::{Store, StoreError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FavoriteOrder {
    FavoritedAt,
    Title,
    Year,
}

#[derive(Clone, Debug)]
pub struct PlaybackViewQuery {
    pub user: UserId,
    pub libraries: Vec<LibraryId>,
    pub limit: usize,
    pub after: Option<String>,
    pub order: FavoriteOrder,
    pub descending: bool,
    pub unwatched_first: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackViewRow {
    pub media_id: MediaId,
    pub library_id: LibraryId,
    /// Most recently favorited unit of the Media.
    pub unit: UnitKey,
    pub favorited_at: i64,
}

#[derive(Clone, Debug, Default)]
pub struct PlaybackViewPage {
    pub items: Vec<PlaybackViewRow>,
    pub total: Option<usize>,
    pub next_cursor: Option<String>,
}

impl Store {
    /// One keyset page of distinct favorited Media in the authorized libraries.
    /// The head page counts the same predicate; continuation pages omit the total.
    /// Cross-library duplicates collapse before LIMIT.
    pub fn playback_favorite_page(
        &self,
        q: &PlaybackViewQuery,
    ) -> Result<PlaybackViewPage, StoreError> {
        self.with_library_read(|db| db.playback_favorite_page(q))
    }

    /// Boundary before a genuine absolute-position jump; rejects an existing cursor.
    pub fn playback_favorite_position(
        &self,
        q: &PlaybackViewQuery,
        start: usize,
    ) -> Result<Option<String>, StoreError> {
        self.with_library_read(|db| db.playback_favorite_position(q, start))
    }
}

#[path = "playback_views/cursor.rs"]
mod cursor;
#[path = "playback_views/query.rs"]
mod query;

#[path = "playback_views/connection.rs"]
mod connection;
