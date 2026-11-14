//! Batch read facts for browse/playback consumers: media by id, and one
//! user's complete playback units.

use std::collections::HashMap;

use domain::{FileCoverage, Media, MediaId, UserId};
use rusqlite::params;

use super::maps::{LIBRARY_FILE_COLUMNS, group_file_coverage, map_file_unit, map_media, parse_id};
use super::media::MEDIA_SELECT;
use super::playback::UnitState;
use super::{LibraryOwnership, Store, StoreError};

/// One playback unit with its media id and full per-unit state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackUnitSnapshot {
    pub media_id: MediaId,
    pub season: i32,
    pub episode: i32,
    pub state: UnitState,
}

pub(super) const MEDIA_BATCH_CHUNK: usize = 500;

impl Store {
    /// Media rows for `ids`; unknown ids are simply absent, and empty input is
    /// an empty map. Ids are chunked to stay under SQLite's variable limit.
    pub fn media_batch(&self, ids: &[MediaId]) -> Result<HashMap<MediaId, Media>, StoreError> {
        self.with_library_read(|db| db.media_batch(ids))
    }

    /// Every playback unit for one user, with the same columns as
    /// `Store::unit_state` (position, play count, and track preferences
    /// included — not a reduced season/episode/played row).
    pub fn playback_units_for_user(
        &self,
        user: UserId,
    ) -> Result<Vec<PlaybackUnitSnapshot>, StoreError> {
        self.with_library_read(|db| db.playback_units_for_user(user))
    }

    /// Playback units for a specific list of media ids, chunked to stay under SQLite variable limits.
    pub fn playback_units_for_media(
        &self,
        user: UserId,
        ids: &[MediaId],
    ) -> Result<Vec<PlaybackUnitSnapshot>, StoreError> {
        self.with_library_read(|db| db.playback_units_for_media(user, ids))
    }

    /// Read files by their persisted Library ownership through the ownership index.
    pub fn ledger_candidates_for_library(
        &self,
        ownership: &LibraryOwnership,
        library_id: &str,
    ) -> Result<Vec<FileCoverage>, StoreError> {
        self.with_library_read(|db| db.ledger_candidates_for_library(ownership, library_id))
    }

    /// Restrict a Jellyfin parent request to ownership candidates before loading
    /// media and playback facts.
    pub fn jellyfin_ledger_candidates(
        &self,
        ownership: &LibraryOwnership,
        parent_id: Option<&str>,
    ) -> Result<Vec<FileCoverage>, StoreError> {
        self.with_library_read(|db| db.jellyfin_ledger_candidates(ownership, parent_id))
    }

    pub fn jellyfin_ledger_candidates_with_vm_steps(
        &self,
        ownership: &LibraryOwnership,
        parent_id: &str,
    ) -> Result<(Vec<FileCoverage>, i64), StoreError> {
        self.with_library_read(|db| {
            db.jellyfin_ledger_candidates_with_vm_steps(ownership, parent_id)
        })
    }
}

#[path = "file_reads/connection.rs"]
mod connection;
