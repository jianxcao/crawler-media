use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, LibraryId,
    MediaId, MediaKind, PhysicalSourceId, QualitySource, TimelineMode, TypedItemId, UnitKey,
    UnitScope, UserId,
};
use rusqlite::{Connection, OptionalExtension, params};

use super::types::{
    BoundPlaybackEvent, EventOutcome, PlaybackEventType, PlaybackMutation, PlaybackTarget,
    ResolvedResume, ResumeStatus,
};
use super::super::{Store, StoreError};

impl Store {
    pub fn resolve_playback(
        &self,
        user: UserId,
        item: &TypedItemId,
        preferred_source: Option<&PhysicalSourceId>,
        explicit_start_ms: Option<i64>,
    ) -> Result<ResolvedResume, StoreError> {
        let (media_id, unit_key) = match item {
            TypedItemId::Work(m) => (*m, UnitKey::WHOLE),
            TypedItemId::Season { media, season } => (*media, UnitKey::new(*season as i32, 1)),
            TypedItemId::Unit { media, key } => (*media, *key),
        };

        let media_id_str = media_id.to_string();
        let allowed_libraries = self.authorized_library_ids(user)?;
        if allowed_libraries.is_empty() {
            return Err(StoreError::Missing(format!("no authorized libraries for unit {unit_key:?}")));
        }
        let lib_in_clause = allowed_libraries
            .iter()
            .map(|l| format!("'{l}'"))
            .collect::<Vec<_>>()
            .join(",");

        // Find preferred file or active file for this unit, validating presence, publication, and revision
        let file_row = match preferred_source {
            Some(src) => {
                let sql = format!(
                    "SELECT f.file_id, f.library_id, f.root_id, f.media_id, f.kind, f.path_key,
                            f.file_revision, f.source_version, f.presence_state, f.publication_state,
                            f.duration_ms
                     FROM library_files f
                     JOIN library_file_units u ON f.file_id = u.file_id
                     WHERE f.file_id = ?1 AND f.file_revision = ?2
                       AND u.media_id = ?3 AND u.season = ?4 AND u.episode = ?5
                       AND f.presence_state = 'present' AND f.publication_state = 'published'
                       AND f.library_id IN ({lib_in_clause})"
                );
                self.library
                    .query_row(
                        &sql,
                        params![
                            src.file_id.to_string(),
                            src.revision,
                            media_id_str,
                            unit_key.season,
                            unit_key.episode,
                        ],
                        |r| {
                            let fid: LedgerId = r.get::<_, String>(0)?.parse().unwrap();
                            let lid: LibraryId = r.get::<_, String>(1)?.parse().unwrap();
                            let mid: MediaId = r.get::<_, String>(3)?.parse().unwrap();
                            let kind: MediaKind = r.get::<_, String>(4)?.parse().unwrap_or(MediaKind::Movie);
                            let path_str: String = r.get(5)?;
                            let pres_str: String = r.get(8)?;
                            let pub_str: String = r.get(9)?;
                            let dur: Option<i64> = r.get(10)?;

                            Ok(LibraryFile {
                                file_id: fid,
                                library_id: lid,
                                root_id: r.get(2)?,
                                media_id: mid,
                                kind,
                                path: path_str.into(),
                                version: FileVersion {
                                    revision: r.get(6)?,
                                    source_version: r.get(7)?,
                                },
                                presence: FilePresence::from_str(&pres_str).unwrap_or(FilePresence::Present),
                                publication: FilePublication::from_str(&pub_str).unwrap_or(FilePublication::Published),
                                size_bytes: None,
                                stat_known: true,
                                created_ns: None,
                                modified_ns: None,
                                source_digest: None,
                                source_path: None,
                                transfer_mode: None,
                                resolution: None,
                                codec: None,
                                hdr: None,
                                quality_source: QualitySource::Probe,
                                confidence: Confidence::High,
                                filter_score: None,
                                release_quality: None,
                                duration_ms: dur,
                                tracks_version: 1,
                                poster_path: None,
                                backdrop_path: None,
                                still_path: None,
                            })
                        },
                    )
                    .optional()?
            }
            None => None,
        };

        let file = match file_row {
            Some(f) => f,
            None => {
                let sql = format!(
                    "SELECT f.file_id, f.library_id, f.root_id, f.media_id, f.kind, f.path_key,
                            f.file_revision, f.source_version, f.presence_state, f.publication_state,
                            f.duration_ms
                     FROM library_files f
                     JOIN library_file_units u ON f.file_id = u.file_id
                     WHERE u.media_id = ?1 AND u.season = ?2 AND u.episode = ?3
                       AND f.presence_state = 'present' AND f.publication_state = 'published'
                       AND f.library_id IN ({lib_in_clause})
                     ORDER BY f.file_revision DESC, f.file_id ASC
                     LIMIT 1"
                );
                self.library
                    .query_row(
                        &sql,
                        params![media_id_str, unit_key.season, unit_key.episode],
                        |r| {
                            let fid: LedgerId = r.get::<_, String>(0)?.parse().unwrap();
                            let lid: LibraryId = r.get::<_, String>(1)?.parse().unwrap();
                            let mid: MediaId = r.get::<_, String>(3)?.parse().unwrap();
                            let kind: MediaKind = r.get::<_, String>(4)?.parse().unwrap_or(MediaKind::Movie);
                            let path_str: String = r.get(5)?;
                            let pres_str: String = r.get(8)?;
                            let pub_str: String = r.get(9)?;
                            let dur: Option<i64> = r.get(10)?;

                            Ok(LibraryFile {
                                file_id: fid,
                                library_id: lid,
                                root_id: r.get(2)?,
                                media_id: mid,
                                kind,
                                path: path_str.into(),
                                version: FileVersion {
                                    revision: r.get(6)?,
                                    source_version: r.get(7)?,
                                },
                                presence: FilePresence::from_str(&pres_str).unwrap_or(FilePresence::Present),
                                publication: FilePublication::from_str(&pub_str).unwrap_or(FilePublication::Published),
                                size_bytes: None,
                                stat_known: true,
                                created_ns: None,
                                modified_ns: None,
                                source_digest: None,
                                source_path: None,
                                transfer_mode: None,
                                resolution: None,
                                codec: None,
                                hdr: None,
                                quality_source: QualitySource::Probe,
                                confidence: Confidence::High,
                                filter_score: None,
                                release_quality: None,
                                duration_ms: dur,
                                tracks_version: 1,
                                poster_path: None,
                                backdrop_path: None,
                                still_path: None,
                            })
                        },
                    )
                    .optional()?
                    .ok_or_else(|| StoreError::Missing(format!("playable file for unit {unit_key:?}")))?
            }
        };

        let target = PlaybackTarget {
            media_id,
            unit: unit_key,
            file,
            config_revision: 0,
            timeline: TimelineMode::FullFile,
        };
        let file_dur_ms = target.file.duration_ms;
        if let Some(start) = explicit_start_ms {
            return Ok(ResolvedResume {
                target,
                start_ms: start,
                duration_ms: file_dur_ms,
                audio_track: None,
                subtitle_track: None,
                status: ResumeStatus::Compatible,
            });
        }

        // Check playback_units table
        let p_sql = "SELECT played, position_ms, duration_ms, resume_file_id, resume_file_revision,
                            audio_track, subtitle_track
                     FROM playback_units
                     WHERE user_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4";
        let user_id_str = user.to_string();
        let row_opt = self.library.query_row(
            p_sql,
            params![user_id_str, media_id_str, unit_key.season, unit_key.episode],
            |r| {
                Ok((
                    r.get::<_, i64>(0)? != 0,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                ))
            },
        ).optional()?;

        let Some((played, pos_ms, dur_ms, res_fid, res_rev, a_track, s_track)) = row_opt else {
            return Ok(ResolvedResume {
                target,
                start_ms: 0,
                duration_ms: file_dur_ms,
                audio_track: None,
                subtitle_track: None,
                status: ResumeStatus::NoProgress,
            });
        };

        if played {
            return Ok(ResolvedResume {
                target,
                start_ms: 0,
                duration_ms: dur_ms.or(file_dur_ms),
                audio_track: a_track,
                subtitle_track: s_track,
                status: ResumeStatus::AlreadyPlayed,
            });
        }

        if pos_ms == 0 {
            return Ok(ResolvedResume {
                target,
                start_ms: 0,
                duration_ms: dur_ms.or(file_dur_ms),
                audio_track: a_track,
                subtitle_track: s_track,
                status: ResumeStatus::NoProgress,
            });
        }

        let is_same_source = match (res_fid, res_rev) {
            (Some(f), Some(r)) => f == target.file.file_id.to_string() && r == target.file.version.revision,
            _ => false,
        };

        let status = if is_same_source {
            ResumeStatus::Compatible
        } else {
            ResumeStatus::SourceChanged
        };

        Ok(ResolvedResume {
            target,
            start_ms: pos_ms,
            duration_ms: dur_ms.or(file_dur_ms),
            audio_track: a_track,
            subtitle_track: s_track,
            status,
        })
    }

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

        tx.commit()?;
        Ok(())
    }

    pub fn commit_playback_event(
        &self,
        event: &BoundPlaybackEvent,
        now_ms: i64,
    ) -> Result<EventOutcome, StoreError> {
        let tx = self.library.unchecked_transaction()?;
        let (media_id, unit_key) = match &event.item {
            TypedItemId::Work(m) => (*m, UnitKey::WHOLE),
            TypedItemId::Season { media, season } => (*media, UnitKey::new(*season as i32, 1)),
            TypedItemId::Unit { media, key } => (*media, *key),
        };

        let user_id_str = event.user_id.to_string();
        let media_id_str = media_id.to_string();
        let file_id_str = event.source_id.file_id.to_string();

        let played_threshold = event.duration_ms.map(|d| (d as f64 * 0.9) as i64).unwrap_or(i64::MAX);
        let reached_played = event.position_ms >= played_threshold;

        let sql = "INSERT INTO playback_units
                    (user_id, media_id, season, episode, played, has_played_assertion,
                     played_changed_at_ms, position_ms, duration_ms, resume_file_id,
                     resume_file_revision, resume_source_version, timeline_mode,
                     watch_activity_at, audio_track, subtitle_track)
                   VALUES
                    (?1, ?2, ?3, ?4, ?5, ?5, CASE WHEN ?5 = 1 THEN ?6 ELSE NULL END,
                     ?7, ?8, ?9, ?10, ?11, 'full_file', ?6, ?12, ?13)
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
                     audio_track = ?12,
                     subtitle_track = ?13";

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
            event.audio_track,
            event.subtitle_track,
        ])?;

        tx.commit()?;
        Ok(EventOutcome::Applied)
    }

    pub fn begin_media_merge(
        &self,
        request: &super::types::MediaMergeRequest,
        now_ms: i64,
    ) -> Result<String, StoreError> {
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
        let tx = self.library.unchecked_transaction()?;
        let (state, survivor_str, loser_str): (String, String, Option<String>) = tx.query_row(
            "SELECT state, survivor_id, loser_id FROM media_operations WHERE operation_id = ?1",
            params![operation_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;

        if state == "succeeded" {
            return Ok(super::types::OperationStatus {
                operation_id: operation_id.to_string(),
                phase: "complete".into(),
                state: "succeeded".into(),
                error: None,
            });
        }

        if let Some(loser) = loser_str {
            // Delete loser's derived units and items first so foreign keys don't break
            tx.execute("DELETE FROM library_units WHERE media_id = ?1", params![loser])?;
            tx.execute("DELETE FROM library_items WHERE media_id = ?1", params![loser])?;

            // Reassign loser's files to survivor
            tx.execute(
                "UPDATE library_files SET media_id = ?1 WHERE media_id = ?2",
                params![survivor_str, loser],
            )?;
            tx.execute(
                "UPDATE library_file_units SET media_id = ?1 WHERE media_id = ?2",
                params![survivor_str, loser],
            )?;
            // Mark loser as retired pointing to survivor
            tx.execute(
                "UPDATE media SET identity_state = 'retired', survivor_id = ?1 WHERE media_id = ?2",
                params![survivor_str, loser],
            )?;
        }

        tx.execute(
            "UPDATE media_operations SET state = 'succeeded', phase_cursor = 'complete', updated_at_ms = ?1 WHERE operation_id = ?2",
            params![now_ms, operation_id],
        )?;

        tx.commit()?;
        Ok(super::types::OperationStatus {
            operation_id: operation_id.to_string(),
            phase: "complete".into(),
            state: "succeeded".into(),
            error: None,
        })
    }
}
