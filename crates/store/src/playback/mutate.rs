use domain::{
    LibraryFile, MediaId, PhysicalSourceId, TimelineMode, TypedItemId, UnitKey, UnitScope, UserId,
};
use rusqlite::{OptionalExtension, params};

use super::source::PlaybackSource;
use super::user_summary::SummaryScope;
use super::types::{
    BoundPlaybackEvent, EventOutcome, PlaybackMutation, PlaybackTarget, ResolvedResume,
    ResumeStatus,
};
use super::super::{Store, StoreError};

/// Resume facts stored on `playback_units`, with the source binding they were
/// written against.
#[derive(Debug)]
struct StoredResume {
    played: bool,
    position_ms: i64,
    duration_ms: Option<i64>,
    audio_track: Option<String>,
    subtitle_track: Option<String>,
    binding: Option<StoredBinding>,
}

/// Which source a stored resume belongs to. The five columns are written
/// all-or-nothing (schema CHECK), so either all are present or the row carries
/// no resume package at all.
#[derive(Debug)]
struct StoredBinding {
    file_id: String,
    revision: i64,
    source_version: String,
    timeline_mode: String,
    tracks_version: i64,
}

impl StoredBinding {
    /// R1: a stored position is reused only when the file, its revision, its
    /// source version, the timeline mode and the track revision all still match
    /// the source we are about to play.
    fn matches(&self, source: &LibraryFile, tracks_version: i64) -> bool {
        self.file_id == source.file_id.to_string()
            && self.revision == source.version.revision
            && self.source_version == source.version.source_version
            && self.timeline_mode == TimelineMode::FullFile.as_str()
            && self.tracks_version == tracks_version
    }
}

/// How a stored resume relates to the selected source.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Binding {
    /// No stored resume package: the item has no progress to resume.
    Absent,
    /// The stored resume belongs to the selected source.
    Matches,
    /// The stored resume belongs to another source, revision or track set.
    Changed,
}

/// Watch unit coordinates of a logical item. Movies and videos are the whole
/// file `(-1, -1)`; a season item plays its first episode.
fn playback_unit(item: &TypedItemId) -> (MediaId, UnitKey) {
    match item {
        TypedItemId::Work(media) => (*media, UnitKey::WHOLE),
        TypedItemId::Season { media, season } => (*media, UnitKey::new(*season as i32, 1)),
        TypedItemId::Unit { media, key } => (*media, *key),
    }
}

impl Store {
    /// Resume the caller's own progress for a logical item.
    ///
    /// The source is picked through `library_files JOIN library_file_units`
    /// (published + present, authorized libraries) and the stored position is
    /// only reused when its whole binding still matches that source; otherwise
    /// the result is `SourceChanged` with `start_ms = 0`, the current source's
    /// duration and no track selection. An explicit `start_ms` from the client
    /// always wins.
    pub fn resolve_playback(
        &self,
        user: UserId,
        item: &TypedItemId,
        preferred_source: Option<&PhysicalSourceId>,
        explicit_start_ms: Option<i64>,
    ) -> Result<ResolvedResume, StoreError> {
        let (media_id, unit) = playback_unit(item);
        let allowed = self.authorized_library_ids(user)?;
        let source = self
            .best_source_for_unit(&allowed, media_id, unit, preferred_source)?
            .ok_or_else(|| StoreError::Missing(format!("no playable source for unit {unit:?}")))?;
        let stored = self.stored_resume(user, media_id, unit)?;
        Ok(resolve_resume(media_id, unit, source, stored, explicit_start_ms))
    }

    /// Resume facts for one watch unit; `None` when the user never played it.
    fn stored_resume(
        &self,
        user: UserId,
        media_id: MediaId,
        unit: UnitKey,
    ) -> Result<Option<StoredResume>, StoreError> {
        self.library
            .query_row(
                "SELECT played, position_ms, duration_ms, audio_track, subtitle_track,
                        resume_file_id, resume_file_revision, resume_source_version, timeline_mode,
                        tracks_version
                 FROM playback_units
                 WHERE user_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4",
                params![
                    user.to_string(),
                    media_id.to_string(),
                    unit.season,
                    unit.episode
                ],
                |row| {
                    let binding = match (
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, Option<i64>>(9)?,
                    ) {
                        (
                            Some(file_id),
                            Some(revision),
                            Some(source_version),
                            Some(timeline_mode),
                            Some(tracks_version),
                        ) => Some(StoredBinding {
                            file_id,
                            revision,
                            source_version,
                            timeline_mode,
                            tracks_version,
                        }),
                        _ => None,
                    };
                    Ok(StoredResume {
                        played: row.get::<_, i64>(0)? != 0,
                        position_ms: row.get(1)?,
                        duration_ms: row.get(2)?,
                        audio_track: row.get(3)?,
                        subtitle_track: row.get(4)?,
                        binding,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }
}

/// Pure R1 decision: duration and tracks always describe the selected source
/// (never the previous one), and a changed source never keeps the old position.
fn resolve_resume(
    media_id: MediaId,
    unit: UnitKey,
    source: PlaybackSource,
    stored: Option<StoredResume>,
    explicit_start_ms: Option<i64>,
) -> ResolvedResume {
    let binding = match stored.as_ref().map(|resume| resume.binding.as_ref()) {
        Some(Some(binding)) if binding.matches(&source.file, source.tracks_version) => {
            Binding::Matches
        }
        Some(Some(_)) => Binding::Changed,
        _ => Binding::Absent,
    };
    let (audio_track, subtitle_track, stored_duration) = match (binding, stored.as_ref()) {
        (Binding::Matches, Some(resume)) => (
            resume.audio_track.clone(),
            resume.subtitle_track.clone(),
            resume.duration_ms,
        ),
        _ => (None, None, None),
    };
    // The selected source decides the duration; a stored duration is only
    // trusted while that same source (and its track revision) is still bound.
    let duration_ms = source.file.duration_ms.or(stored_duration);
    let target = PlaybackTarget {
        media_id,
        unit,
        file: source.file,
        config_revision: 0,
        timeline: TimelineMode::FullFile,
    };
    let changed_status = match binding {
        Binding::Changed => ResumeStatus::SourceChanged,
        _ => ResumeStatus::Compatible,
    };
    // A client that names a position (a seek, or another file version it just
    // picked) is always honoured.
    if let Some(start_ms) = explicit_start_ms {
        return ResolvedResume {
            target,
            start_ms,
            duration_ms,
            audio_track,
            subtitle_track,
            status: changed_status,
        };
    }
    let Some(resume) = stored else {
        return no_progress(target, duration_ms, audio_track, subtitle_track);
    };
    if binding == Binding::Changed {
        return ResolvedResume {
            target,
            start_ms: 0,
            duration_ms,
            audio_track: None,
            subtitle_track: None,
            status: ResumeStatus::SourceChanged,
        };
    }
    if resume.played {
        return ResolvedResume {
            target,
            start_ms: 0,
            duration_ms,
            audio_track,
            subtitle_track,
            status: ResumeStatus::AlreadyPlayed,
        };
    }
    if resume.position_ms == 0 {
        return no_progress(target, duration_ms, audio_track, subtitle_track);
    }
    ResolvedResume {
        target,
        start_ms: resume.position_ms,
        duration_ms,
        audio_track,
        subtitle_track,
        status: ResumeStatus::Compatible,
    }
}

fn no_progress(
    target: PlaybackTarget,
    duration_ms: Option<i64>,
    audio_track: Option<String>,
    subtitle_track: Option<String>,
) -> ResolvedResume {
    ResolvedResume {
        target,
        start_ms: 0,
        duration_ms,
        audio_track,
        subtitle_track,
        status: ResumeStatus::NoProgress,
    }
}

impl Store {
    pub fn mutate_playback(
        &self,
        media: MediaId,
        change: &PlaybackMutation,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        let tx = self.library.unchecked_transaction()?;
        let (season, episode) = match change.scope {
            UnitScope::Whole => (-1, -1),
            UnitScope::Season(s) => (s as i32, -1),
            UnitScope::Episode { season, episode } => (season as i32, episode as i32),
        };

        let user_id_str = change.user_id.to_string();
        let media_id_str = media.to_string();

        let played_val = change.played.map(|p| if p { 1 } else { 0 });
        let fav_val = change.favorite.map(|f| if f { 1 } else { 0 });

        let sql = "INSERT INTO playback_units
                    (user_id, media_id, season, episode, played, has_played_assertion,
                     played_changed_at_ms, favorite, favorite_at, watch_activity_at)
                   VALUES
                    (?1, ?2, ?3, ?4, COALESCE(?5, 0), CASE WHEN ?5 IS NOT NULL THEN 1 ELSE 0 END,
                     CASE WHEN ?5 IS NOT NULL THEN ?6 ELSE NULL END, COALESCE(?7, 0),
                     CASE WHEN ?7 = 1 THEN ?6 ELSE 0 END, ?6)
                   ON CONFLICT(user_id, media_id, season, episode) DO UPDATE SET
                     played = COALESCE(?5, playback_units.played),
                     has_played_assertion = CASE WHEN ?5 IS NOT NULL THEN 1 ELSE playback_units.has_played_assertion END,
                     played_changed_at_ms = CASE WHEN ?5 IS NOT NULL THEN ?6 ELSE playback_units.played_changed_at_ms END,
                     favorite = COALESCE(?7, playback_units.favorite),
                     favorite_at = CASE WHEN ?7 = 1 THEN ?6 ELSE playback_units.favorite_at END,
                     watch_activity_at = ?6";

        tx.execute(sql, params![
            user_id_str,
            media_id_str,
            season,
            episode,
            played_val,
            now_ms,
            fav_val,
        ])?;

        self.refresh_user_library_summary(
            change.user_id,
            media,
            SummaryScope::for_unit(season, episode),
        )?;

        tx.commit()?;
        Ok(())
    }

    pub fn commit_playback_event(
        &self,
        event: &BoundPlaybackEvent,
        now_ms: i64,
    ) -> Result<EventOutcome, StoreError> {
        let tx = self.library.unchecked_transaction()?;
        let (media_id, unit_key) = playback_unit(&event.item);

        let user_id_str = event.user_id.to_string();
        let media_id_str = media_id.to_string();
        let file_id_str = event.source_id.file_id.to_string();
        // The resume binding records the track revision it was written for, so a
        // later resolve can tell whether the source (and its track set) is still
        // the same. A file that is no longer a published, present source writes
        // no track names at all.
        let tracks_version = self.file_tracks_version(event.source_id.file_id)?;

        let played_threshold = event.duration_ms.map(|d| (d as f64 * 0.9) as i64).unwrap_or(i64::MAX);
        let reached_played = event.position_ms >= played_threshold;

        let sql = "INSERT INTO playback_units
                    (user_id, media_id, season, episode, played, has_played_assertion,
                     played_changed_at_ms, position_ms, duration_ms, resume_file_id,
                     resume_file_revision, resume_source_version, timeline_mode,
                     watch_activity_at, tracks_version, audio_track, subtitle_track)
                   VALUES
                    (?1, ?2, ?3, ?4, ?5, ?5, CASE WHEN ?5 = 1 THEN ?6 ELSE NULL END,
                     ?7, ?8, ?9, ?10, ?11, 'full_file', ?6, ?12,
                     CASE WHEN ?12 IS NULL THEN NULL ELSE ?13 END,
                     CASE WHEN ?12 IS NULL THEN NULL ELSE ?14 END)
                   ON CONFLICT(user_id, media_id, season, episode) DO UPDATE SET
                     played = CASE WHEN ?5 = 1 THEN 1 ELSE playback_units.played END,
                     has_played_assertion = CASE WHEN ?5 = 1 THEN 1 ELSE playback_units.has_played_assertion END,
                     played_changed_at_ms = CASE WHEN ?5 = 1 THEN ?6 ELSE playback_units.played_changed_at_ms END,
                     position_ms = ?7,
                     duration_ms = ?8,
                     resume_file_id = ?9,
                     resume_file_revision = ?10,
                     resume_source_version = ?11,
                     timeline_mode = 'full_file',
                     watch_activity_at = ?6,
                     tracks_version = ?12,
                     audio_track = CASE WHEN ?12 IS NULL THEN NULL ELSE ?13 END,
                     subtitle_track = CASE WHEN ?12 IS NULL THEN NULL ELSE ?14 END";

        tx.execute(sql, params![
            user_id_str,
            media_id_str,
            unit_key.season,
            unit_key.episode,
            if reached_played { 1 } else { 0 },
            now_ms,
            event.position_ms,
            event.duration_ms,
            file_id_str,
            event.source_id.revision,
            event.source_version,
            tracks_version,
            event.audio_track,
            event.subtitle_track,
        ])?;

        self.refresh_user_library_summary(
            event.user_id,
            media_id,
            SummaryScope::for_unit(unit_key.season, unit_key.episode),
        )?;

        tx.commit()?;
        Ok(EventOutcome::Applied)
    }

    pub fn begin_media_merge(
        &self,
        request: &super::types::MediaMergeRequest,
        now_ms: i64,
    ) -> Result<String, StoreError> {
        if request.survivor == request.loser {
            return Err(StoreError::Invalid("cannot merge media with itself".into()));
        }
        let tx = self.library.unchecked_transaction()?;
        let plan_json = serde_json::to_string(request)?;

        // Idempotency: check if an operation already exists for this request_id
        let existing_op: Option<String> = tx
            .query_row(
                "SELECT operation_id FROM media_operations WHERE json_extract(plan_json, '$.request_id') = ?1",
                params![request.request_id],
                |r| r.get(0),
            )
            .optional()?;

        if let Some(op_id) = existing_op {
            return Ok(op_id);
        }

        let op_id = format!("op_merge_{}", uuid::Uuid::new_v4().simple());
        tx.execute(
            "INSERT INTO media_operations
                (operation_id, kind, state, survivor_id, loser_id, plan_json, phase_cursor, error, created_at_ms, updated_at_ms)
             VALUES
                (?1, 'merge', 'planned', ?2, ?3, ?4, 'init', NULL, ?5, ?5)",
            params![
                op_id,
                request.survivor.to_string(),
                request.loser.to_string(),
                plan_json,
                now_ms,
            ],
        )?;

        tx.commit()?;
        Ok(op_id)
    }

    pub fn advance_media_merge(
        &self,
        operation_id: &str,
        now_ms: i64,
    ) -> Result<super::types::OperationStatus, StoreError> {
        self.advance_merge_operation(operation_id, now_ms)
    }
}
