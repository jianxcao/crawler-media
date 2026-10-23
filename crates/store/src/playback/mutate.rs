use domain::{
    LibraryFile, MediaId, PhysicalSourceId, TimelineMode, TypedItemId, UnitKey, UnitScope, UserId,
};
use rusqlite::{OptionalExtension, params};

use super::super::{Store, StoreError};
use super::source::PlaybackSource;
use super::types::{
    BoundPlaybackEvent, EventOutcome, PlaybackMutation, PlaybackTarget, ResolvedResume,
    ResumeStatus,
};
use super::user_summary::SummaryScope;

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
    /// source version and the timeline mode still match the source we are about
    /// to play. The track revision does not move the timeline (R13).
    fn matches(&self, source: &LibraryFile) -> bool {
        self.file_id == source.file_id.to_string()
            && self.revision == source.version.revision
            && self.source_version == source.version.source_version
            && self.timeline_mode == TimelineMode::FullFile.as_str()
    }

    /// Track selections are only valid for the track revision they were made on.
    fn tracks_match(&self, tracks_version: i64) -> bool {
        self.tracks_version == tracks_version
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
        let mut stored = self.stored_resume(user, media_id, unit)?;
        // AlreadyPlayed follows the effective (scoped) assertion; the physical
        // resume package still comes only from the unit's own row.
        if let Some(assertion) = self.effective_played_assertion(user, media_id, unit)? {
            match stored.as_mut() {
                Some(resume) => resume.played = assertion.played,
                None if assertion.played => {
                    stored = Some(StoredResume {
                        played: true,
                        position_ms: 0,
                        duration_ms: None,
                        audio_track: None,
                        subtitle_track: None,
                        binding: None,
                    })
                }
                None => {}
            }
        }
        let resume_preferred = stored
            .as_ref()
            .and_then(|s| s.binding.as_ref())
            .and_then(|b| {
                let file_id = b.file_id.parse::<domain::LedgerId>().ok()?;
                Some(PhysicalSourceId {
                    file_id,
                    revision: b.revision,
                })
            });
        let source = if let Some(pref) = preferred_source {
            self.best_source_for_unit(&allowed, media_id, unit, Some(pref))?
        } else if let Some(resume_pref) = &resume_preferred {
            self.best_source_for_unit(&allowed, media_id, unit, Some(resume_pref))?
                .or_else(|| {
                    self.best_source_for_unit(&allowed, media_id, unit, None)
                        .ok()
                        .flatten()
                })
        } else {
            self.best_source_for_unit(&allowed, media_id, unit, None)?
        }
        .ok_or_else(|| StoreError::Missing(format!("no playable source for unit {unit:?}")))?;
        // The target is pinned to the source Library's real configuration revision.
        let config_revision = self.library_config_revision(&source.file.library_id.to_string())?;
        Ok(resolve_resume(
            media_id,
            unit,
            source,
            config_revision,
            stored,
            explicit_start_ms,
        ))
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
    config_revision: i64,
    stored: Option<StoredResume>,
    explicit_start_ms: Option<i64>,
) -> ResolvedResume {
    let stored_binding = stored.as_ref().and_then(|resume| resume.binding.as_ref());
    let binding = match stored_binding {
        Some(binding) if binding.matches(&source.file) => Binding::Matches,
        Some(_) => Binding::Changed,
        None => Binding::Absent,
    };
    let tracks_match = stored_binding.is_some_and(|b| b.tracks_match(source.tracks_version));
    let (audio_track, subtitle_track, stored_duration) = match (binding, stored.as_ref()) {
        (Binding::Matches, Some(resume)) if tracks_match => (
            resume.audio_track.clone(),
            resume.subtitle_track.clone(),
            resume.duration_ms,
        ),
        (Binding::Matches, Some(resume)) => (None, None, resume.duration_ms),
        _ => (None, None, None),
    };
    // The selected source decides the duration; a stored duration is only
    // trusted while that same source (and its track revision) is still bound.
    let duration_ms = source.file.duration_ms.or(stored_duration);
    let target = PlaybackTarget {
        media_id,
        unit,
        file: source.file,
        config_revision,
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
    /// The only logical mark entry (C04/C05). `played` writes one assertion on
    /// exactly the target scope (`played_changed_at_ms` in ms); `favorite`
    /// stamps `favorite_at` in seconds (kept on a repeated favourite, 0 when
    /// cleared; clearing the whole item clears every unit). A mark never moves
    /// `watch_activity_at` or the resume package.
    pub fn mutate_playback(
        &self,
        media: MediaId,
        change: &PlaybackMutation,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        if change.played.is_none() && change.favorite.is_none() {
            return Ok(());
        }
        let tx = crate::write_tx(&self.library)?;
        let (season, episode) = match change.scope {
            UnitScope::Whole => (-1, -1),
            UnitScope::Season(s) => (s as i32, -1),
            UnitScope::Episode { season, episode } => (season as i32, episode as i32),
        };
        let user_id = change.user_id.to_string();
        let media_id = media.to_string();
        let played = change.played.map(i64::from);
        let favorite = change.favorite.map(i64::from);
        tx.execute(
            "INSERT INTO playback_units
                (user_id, media_id, season, episode, played, has_played_assertion,
                 played_changed_at_ms, favorite, favorite_at, watch_activity_at)
             VALUES
                (?1, ?2, ?3, ?4, COALESCE(?5, 0), ?5 IS NOT NULL,
                 CASE WHEN ?5 IS NOT NULL THEN ?6 END, COALESCE(?7, 0),
                 CASE WHEN ?7 = 1 THEN ?8 ELSE 0 END, 0)
             ON CONFLICT(user_id, media_id, season, episode) DO UPDATE SET
                played = COALESCE(?5, played),
                has_played_assertion = CASE WHEN ?5 IS NOT NULL THEN 1 ELSE has_played_assertion END,
                played_changed_at_ms = CASE WHEN ?5 IS NOT NULL THEN ?6 ELSE played_changed_at_ms END,
                favorite = COALESCE(?7, favorite),
                favorite_at = CASE
                    WHEN ?7 = 1 AND favorite = 1 AND favorite_at != 0 THEN favorite_at
                    WHEN ?7 = 1 THEN ?8
                    WHEN ?7 = 0 THEN 0
                    ELSE favorite_at END",
            params![
                user_id,
                media_id,
                season,
                episode,
                played,
                now_ms,
                favorite,
                now_ms / 1000
            ],
        )?;
        if change.favorite == Some(false) && change.scope == UnitScope::Whole {
            tx.execute(
                "UPDATE playback_units SET favorite = 0, favorite_at = 0
                 WHERE user_id = ?1 AND media_id = ?2 AND favorite != 0",
                params![user_id, media_id],
            )?;
        }
        let summary = if change.favorite == Some(false) && change.scope == UnitScope::Whole {
            SummaryScope::Whole
        } else {
            SummaryScope::for_unit(season, episode)
        };
        self.refresh_user_library_summary(change.user_id, media, summary)?;
        tx.commit()?;
        Ok(())
    }

    /// Accept or replay a merge request. The canonical operation id, the pinned
    /// revisions, the lock conflict, the refresh-target decisions and the freeze
    /// of both sides are settled by [`crate::media_operations`] before this
    /// returns, so a refused request has written nothing.
    pub fn begin_media_merge(
        &self,
        request: &super::types::MediaMergeRequest,
        now_ms: i64,
    ) -> Result<String, StoreError> {
        self.begin_merge(request, now_ms)
    }

    pub fn advance_media_merge(
        &self,
        operation_id: &str,
        now_ms: i64,
    ) -> Result<super::types::OperationStatus, StoreError> {
        self.advance_merge_operation(operation_id, now_ms)
    }
}
