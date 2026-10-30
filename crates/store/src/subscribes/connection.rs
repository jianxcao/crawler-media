//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::SubscribeConnection<'_> {
    pub fn list_subscribes_for_user(&self, user_id: UserId) -> Result<Vec<Subscribe>, StoreError> {
        let mut stmt = self.subscribe.prepare(
            "SELECT id, user_id, media_id, coverage_kind, season, episode_from, episode_to,
                    fetch_mode, filter_id, wash_cut, wash_cut_filter_id, full_season_pack, downloader_id,
                    tracking_state, follow_future, search_interval_secs, keep_old_versions, library_id
             FROM subscribes WHERE user_id = ?1",
        )?;
        let rows = stmt.query_map(params![user_id.to_string()], map_subscribe)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_all_subscribes(&self) -> Result<Vec<Subscribe>, StoreError> {
        let mut stmt = self.subscribe.prepare(
            "SELECT id, user_id, media_id, coverage_kind, season, episode_from, episode_to,
                    fetch_mode, filter_id, wash_cut, wash_cut_filter_id, full_season_pack, downloader_id,
                    tracking_state, follow_future, search_interval_secs, keep_old_versions, library_id
             FROM subscribes",
        )?;
        let rows = stmt.query_map([], map_subscribe)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub(super) fn upsert_pending_row(
        &self,
        subscribe_id: SubscribeId,
        score: i32,
        pending: &PendingDownload,
        refresh_submitted_at: bool,
        revive_rejected: bool,
    ) -> Result<(), StoreError> {
        let value_str = pending_torrent_json(pending);
        let submitted_at = pending.submitted_at.unwrap_or_else(super::super::unix_now);
        // H2: automatic rounds never revive a `rejected` row; only an explicit
        // user submission does.
        let next_state = if revive_rejected {
            "'active'"
        } else {
            "CASE WHEN pending_downloads.state IN ('rejected', 'stalled') THEN pending_downloads.state ELSE 'active' END"
        };
        let submitted = if refresh_submitted_at {
            "submitted_at = excluded.submitted_at,"
        } else {
            ""
        };
        let sql = format!(
            "INSERT INTO pending_downloads (subscribe_id, enclosure, title, score, torrent_json, submitted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(subscribe_id, enclosure) DO UPDATE SET
               title = excluded.title,
               score = excluded.score,
               torrent_json = excluded.torrent_json,
               {submitted}
               state = {next_state}"
        );
        let sql = sql.as_str();
        self.subscribe.execute(
            sql,
            params![
                subscribe_id.to_string(),
                pending.torrent.enclosure,
                pending.torrent.title,
                score,
                value_str,
                submitted_at
            ],
        )?;
        Ok(())
    }

    pub fn merge_pending(
        &self,
        subscribe_id: SubscribeId,
        torrents: &[(i32, PendingDownload)],
    ) -> Result<(), StoreError> {
        for (score, pending) in torrents {
            self.upsert_pending_row(subscribe_id, *score, pending, false, false)?;
        }
        Ok(())
    }

    pub fn record_pending_submission(
        &self,
        subscribe_id: SubscribeId,
        score: i32,
        pending: &PendingDownload,
    ) -> Result<(), StoreError> {
        // Single explicit submission (manual delivery): may revive `rejected`.
        self.upsert_pending_row(subscribe_id, score, pending, true, true)
    }

    pub fn record_pending_submissions(
        &self,
        subscribe_id: SubscribeId,
        torrents: &[(i32, PendingDownload)],
    ) -> Result<(), StoreError> {
        for (score, pending) in torrents {
            self.upsert_pending_row(subscribe_id, *score, pending, true, false)?;
        }
        Ok(())
    }

    pub fn record_manual_pending_submissions(
        &self,
        subscribe_id: SubscribeId,
        torrents: &[(i32, PendingDownload)],
    ) -> Result<(), StoreError> {
        for (score, pending) in torrents {
            self.upsert_pending_row(subscribe_id, *score, pending, true, true)?;
        }
        Ok(())
    }

    pub fn confirm_pending_submission(
        &self,
        subscribe_id: SubscribeId,
        enclosure: &str,
        submitted_at: i64,
    ) -> Result<(), StoreError> {
        if submitted_at <= 0 {
            return Err(StoreError::Invalid(
                "submission timestamp must be positive".into(),
            ));
        }
        let changed = self.subscribe.execute(
            "UPDATE pending_downloads SET submitted_at=?3
            WHERE subscribe_id=?1 AND enclosure=?2 AND state='active' AND submitted_at=0",
            params![subscribe_id.to_string(), enclosure, submitted_at],
        )?;
        if changed != 1 {
            return Err(StoreError::Protected(
                "pending submission intent changed".into(),
            ));
        }
        Ok(())
    }

    pub fn mark_pending_rejected(
        &self,
        subscribe_id: SubscribeId,
        enclosures: &[String],
    ) -> Result<(), StoreError> {
        for enclosure in enclosures {
            self.subscribe.execute(
                "UPDATE pending_downloads SET state = 'rejected'
                 WHERE subscribe_id = ?1 AND enclosure = ?2",
                params![subscribe_id.to_string(), enclosure],
            )?;
        }
        Ok(())
    }

    pub fn load_rejected_enclosures(
        &self,
        subscribe_id: SubscribeId,
    ) -> Result<std::collections::HashSet<String>, StoreError> {
        let mut stmt = self.subscribe.prepare(
            "SELECT enclosure FROM pending_downloads WHERE subscribe_id = ?1 AND state IN ('rejected', 'stalled')",
        )?;
        let rows = stmt.query_map(params![subscribe_id.to_string()], |row| {
            row.get::<_, String>(0)
        })?;
        let mut out = std::collections::HashSet::new();
        for row in rows {
            out.insert(row?);
        }
        Ok(out)
    }

    pub fn mark_pending_orphaned(&self, downloader_id: DownloaderId) -> Result<usize, StoreError> {
        Ok(self.subscribe.execute(
            "UPDATE pending_downloads SET state = 'orphan'
             WHERE state = 'active' AND json_extract(torrent_json, '$.downloader_id') = ?1",
            params![downloader_id.to_string()],
        )?)
    }

    pub fn delete_pending(
        &self,
        subscribe_id: SubscribeId,
        enclosure: &str,
    ) -> Result<(), StoreError> {
        self.subscribe.execute(
            "DELETE FROM pending_downloads WHERE subscribe_id = ?1 AND enclosure = ?2",
            params![subscribe_id.to_string(), enclosure],
        )?;
        Ok(())
    }

    pub fn load_pending(
        &self,
        subscribe_id: SubscribeId,
    ) -> Result<Vec<(i32, PendingDownload)>, StoreError> {
        self.load_pending_state(subscribe_id, "active")
    }

    pub fn load_pending_state(
        &self,
        subscribe_id: SubscribeId,
        state: &str,
    ) -> Result<Vec<(i32, PendingDownload)>, StoreError> {
        let mut stmt = self.subscribe.prepare(
            "SELECT score, torrent_json, submitted_at FROM pending_downloads
             WHERE subscribe_id = ?1 AND state = ?2",
        )?;
        let rows = stmt.query_map(params![subscribe_id.to_string(), state], |row| {
            Ok((
                row.get::<_, i32>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (score, raw, submitted_at) = row?;
            let value: serde_json::Value = serde_json::from_str(&raw)?;
            out.push((
                score,
                PendingDownload {
                    torrent: torrent_from_json(&value)?,
                    release_override: release_from_json(value.get("release_override")),
                    downloader_id: value
                        .get("downloader_id")
                        .and_then(|id| id.as_str())
                        .map(DownloaderId::from_str)
                        .transpose()?,
                    submitted_at: (submitted_at > 0).then_some(submitted_at),
                },
            ));
        }
        Ok(out)
    }

    pub fn mark_pending_imported(
        &self,
        subscribe_id: SubscribeId,
        enclosures: &[String],
    ) -> Result<(), StoreError> {
        for enclosure in enclosures {
            self.subscribe.execute(
                "UPDATE pending_downloads SET state = 'imported'
                 WHERE subscribe_id = ?1 AND enclosure = ?2",
                params![subscribe_id.to_string(), enclosure],
            )?;
        }
        Ok(())
    }

    pub fn list_all_pending_routed(
        &self,
    ) -> Result<Vec<(SubscribeId, i32, PendingDownload)>, StoreError> {
        let mut stmt = self.subscribe.prepare(
            "SELECT subscribe_id, score, torrent_json, submitted_at FROM pending_downloads WHERE state = 'active'",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i32>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (subscribe_id, score, raw, submitted_at) = row?;
            let value: serde_json::Value = serde_json::from_str(&raw)?;
            out.push((
                SubscribeId::from_str(&subscribe_id)?,
                score,
                PendingDownload {
                    torrent: torrent_from_json(&value)?,
                    release_override: release_from_json(value.get("release_override")),
                    downloader_id: value
                        .get("downloader_id")
                        .and_then(|id| id.as_str())
                        .map(DownloaderId::from_str)
                        .transpose()?,
                    submitted_at: (submitted_at > 0).then_some(submitted_at),
                },
            ));
        }
        Ok(out)
    }

    pub fn list_all_pending(&self) -> Result<Vec<(SubscribeId, i32, Torrent)>, StoreError> {
        Ok(self
            .list_all_pending_routed()?
            .into_iter()
            .map(|(id, score, pending)| (id, score, pending.torrent))
            .collect())
    }

    pub fn get_subscribe(&self, id: SubscribeId) -> Result<Option<Subscribe>, StoreError> {
        self.subscribe
            .query_row(
                "SELECT id, user_id, media_id, coverage_kind, season, episode_from, episode_to,
                        fetch_mode, filter_id, wash_cut, wash_cut_filter_id, full_season_pack, downloader_id,
                        tracking_state, follow_future, search_interval_secs, keep_old_versions, library_id
                 FROM subscribes WHERE id = ?1",
                params![id.to_string()],
                map_subscribe,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn update_subscribe(&self, subscribe: &Subscribe) -> Result<(), StoreError> {
        let (coverage_kind, season, episode_from, episode_to) = match &subscribe.coverage {
            Coverage::Movie => ("movie", None, None, None),
            Coverage::Tv {
                season,
                episode_from,
                episode_to,
            } => (
                "tv",
                Some(*season as i64),
                Some(*episode_from as i64),
                episode_to.map(|v| v as i64),
            ),
        };
        self.subscribe.execute(
            "UPDATE subscribes SET
                fetch_mode = ?1, filter_id = ?2, wash_cut = ?3, wash_cut_filter_id = ?4,
                full_season_pack = ?5, downloader_id = ?6,
                coverage_kind = ?7, season = ?8, episode_from = ?9, episode_to = ?10,
                tracking_state = ?12, follow_future = ?13, search_interval_secs = ?14,
                keep_old_versions = ?16, library_id = ?17, updated_at = ?15
             WHERE id = ?11",
            params![
                subscribe.fetch_mode.as_str(),
                subscribe.filter_id.to_string(),
                subscribe.wash_cut as i64,
                subscribe.wash_cut_filter_id.map(|id| id.to_string()),
                subscribe.full_season_pack as i64,
                subscribe.downloader_id.map(|id| id.to_string()),
                coverage_kind,
                season,
                episode_from,
                episode_to,
                subscribe.id.to_string(),
                subscribe.tracking_state,
                subscribe.follow_future as i64,
                subscribe.search_interval_secs as i64,
                super::super::now_rfc3339(),
                subscribe.keep_old_versions as i64,
                subscribe.library_id.map(|id| id.to_string()),
            ],
        )?;
        Ok(())
    }

    pub fn subscribe_times(&self, id: SubscribeId) -> Result<(String, String), StoreError> {
        self.subscribe
            .query_row(
                "SELECT created_at, updated_at FROM subscribes WHERE id = ?1",
                params![id.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(Into::into)
    }

    pub fn subscribe_ids_for_user(
        &self,
        user_id: domain::UserId,
    ) -> Result<std::collections::HashSet<domain::SubscribeId>, StoreError> {
        let mut statement = self
            .subscribe
            .prepare("SELECT id FROM subscribes WHERE user_id=?1")?;
        let values = statement
            .query_map([user_id.to_string()], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        values
            .into_iter()
            .map(|value| value.parse().map_err(Into::into))
            .collect()
    }
}

impl crate::LibraryConnection<'_> {
    pub fn playback_marks(
        &self,
        user_id: UserId,
        media_id: MediaId,
    ) -> Result<Option<(bool, bool)>, StoreError> {
        let rows = self.unit_rows(user_id, media_id)?;
        if rows.is_empty() {
            return Ok(None);
        }
        Ok(Some((
            rows.iter().any(|row| row.played),
            rows.iter().any(|row| row.favorite),
        )))
    }

    pub fn playback_unit_progress(
        &self,
        user_id: UserId,
        media_id: MediaId,
        season: Option<u32>,
        episode: Option<u32>,
    ) -> Result<Option<i64>, StoreError> {
        let s: i32 = season.map(|v| v as i32).unwrap_or(-1);
        let e: i32 = episode.map(|v| v as i32).unwrap_or(-1);
        let pos = self
            .library
            .query_row(
                "SELECT position_ms FROM playback_units
                 WHERE user_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4",
                params![user_id.to_string(), media_id.to_string(), s, e],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()
            .map_err(StoreError::from)?;
        Ok(pos.flatten())
    }

    pub fn playback_progress(
        &self,
        user_id: UserId,
        media_id: MediaId,
    ) -> Result<Option<i64>, StoreError> {
        let max = self
            .library
            .query_row(
                "SELECT MAX(position_ms) FROM playback_units
                 WHERE user_id = ?1 AND media_id = ?2",
                params![user_id.to_string(), media_id.to_string()],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()
            .map_err(StoreError::from)?;
        Ok(max.flatten())
    }
}
