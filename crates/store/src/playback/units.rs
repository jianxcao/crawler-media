use domain::{MediaId, UnitKey, UserId};
use rusqlite::{OptionalExtension, params};
use std::str::FromStr;

use super::unit_resume::{ResumeProgress, UnitIdentity};
use super::user_summary::SummaryScope;
use super::{Store, StoreError};
use super::{UnitRow, UnitState};

impl Store {
    pub fn file_owner_library(
        &self,
        file_id: domain::LedgerId,
    ) -> Result<Option<crate::Library>, StoreError> {
        self.with_library_read(|db| db.file_owner_library(file_id))
    }

    pub fn file_covers_unit(
        &self,
        file_id: domain::LedgerId,
        media_id: MediaId,
        unit: UnitKey,
    ) -> Result<bool, StoreError> {
        self.with_library_read(|db| db.file_covers_unit(file_id, media_id, unit))
    }

    pub fn unit_state(
        &self,
        user_id: UserId,
        media_id: MediaId,
        season: i32,
        episode: i32,
    ) -> Result<Option<UnitState>, StoreError> {
        self.with_library_read(|db| db.unit_state(user_id, media_id, season, episode))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upsert_unit(
        &self,
        user_id: UserId,
        media_id: MediaId,
        season: i32,
        episode: i32,
        position_ms: i64,
        played: Option<bool>,
        favorite: Option<bool>,
        duration_ms: Option<i64>,
        audio_track: Option<&str>,
        subtitle_track: Option<&str>,
        increment_play_count: bool,
        now: i64,
    ) -> Result<UnitState, StoreError> {
        let source = self.best_source_for_unit(
            &self.authorized_library_ids(user_id)?,
            media_id,
            UnitKey::new(season, episode),
            None,
        )?;
        // A played/favourite mark is a logical fact and must not require a file. A write that
        // claims resume state (position, duration, tracks) needs a bound source; without one the
        // claim is dropped instead of persisting a position that names no physical file.
        let (source, position_ms, duration_ms, audio_track, subtitle_track) = match source {
            Some(source) => (
                Some(domain::PhysicalSourceId {
                    file_id: source.file.file_id,
                    revision: source.file.version.revision,
                }),
                position_ms,
                duration_ms,
                audio_track,
                subtitle_track,
            ),
            None => (None, 0, None, None, None),
        };
        self.upsert_unit_bound(
            user_id,
            media_id,
            UnitKey::new(season, episode),
            source,
            position_ms,
            played,
            favorite,
            duration_ms,
            audio_track,
            subtitle_track,
            increment_play_count,
            now,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upsert_unit_for_source(
        &self,
        user_id: UserId,
        media_id: MediaId,
        unit: UnitKey,
        source: domain::PhysicalSourceId,
        position_ms: i64,
        played: Option<bool>,
        favorite: Option<bool>,
        duration_ms: Option<i64>,
        audio_track: Option<&str>,
        subtitle_track: Option<&str>,
        increment_play_count: bool,
        now: i64,
    ) -> Result<UnitState, StoreError> {
        self.with_library_write(|db| {
            db.upsert_unit_for_source(
                user_id,
                media_id,
                unit,
                source,
                position_ms,
                played,
                favorite,
                duration_ms,
                audio_track,
                subtitle_track,
                increment_play_count,
                now,
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn upsert_unit_bound(
        &self,
        user_id: UserId,
        media_id: MediaId,
        unit: UnitKey,
        source: Option<domain::PhysicalSourceId>,
        position_ms: i64,
        played: Option<bool>,
        favorite: Option<bool>,
        duration_ms: Option<i64>,
        audio_track: Option<&str>,
        subtitle_track: Option<&str>,
        increment_play_count: bool,
        now: i64,
    ) -> Result<UnitState, StoreError> {
        self.with_library_write(|db| {
            db.upsert_unit_bound(
                user_id,
                media_id,
                unit,
                source,
                position_ms,
                played,
                favorite,
                duration_ms,
                audio_track,
                subtitle_track,
                increment_play_count,
                now,
            )
        })
    }

    /// All distinct media ids and their most recent activity timestamp for a user.
    pub fn user_active_media_ids(
        &self,
        user_id: UserId,
    ) -> Result<Vec<(MediaId, i64)>, StoreError> {
        self.with_library_read(|db| db.user_active_media_ids(user_id))
    }

    /// In-progress units (`position_ms > 0 AND played = 0`), newest first.
    ///
    /// This is the raw unit only. A newer season or show mark can still make
    /// the episode effectively played, so callers that page a resume list must
    /// apply that check before taking `limit` rows.
    pub fn user_resumable_units(
        &self,
        user_id: UserId,
    ) -> Result<Vec<(MediaId, i32, i32, i64, i64)>, StoreError> {
        self.with_library_read(|db| db.user_resumable_units(user_id))
    }

    /// Favourited units ordered by favourite activity, newest first.
    /// `favorite_at = 0` is an unknown historical time and sorts last; readers
    /// must not substitute the still-changing playback clock.
    pub fn favorite_rows(
        &self,
        user_id: UserId,
    ) -> Result<Vec<(MediaId, i64, i32, i32)>, StoreError> {
        self.with_library_read(|db| db.favorite_rows(user_id))
    }

    /// All unit rows of a media (for marks aggregation / cascade).
    pub fn unit_rows(
        &self,
        user_id: UserId,
        media_id: MediaId,
    ) -> Result<Vec<UnitRow>, StoreError> {
        self.with_library_read(|db| db.unit_rows(user_id, media_id))
    }

    /// Clear viewing facts, retaining favorites, duration and track preferences.
    pub fn clear_units(
        &self,
        user_id: UserId,
        media_ids: Option<&[MediaId]>,
        since_ms: Option<i64>,
    ) -> Result<usize, StoreError> {
        self.with_library_write(|db| db.clear_units(user_id, media_ids, since_ms))
    }
}

#[path = "units/connection.rs"]
mod connection;
