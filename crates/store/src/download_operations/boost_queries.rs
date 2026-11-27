use std::collections::HashSet;

use domain::{DownloaderId, SiteId};
use rusqlite::{OptionalExtension, params};

use super::boost::{RES_COLS, TF_COLS, map_reservation, map_task_fact};
use super::boost_facts::fetch_tf_by_id;
use super::types::*;
use super::validate_non_empty;
use crate::plugins::PluginError;
use crate::{Db, Store, StoreError};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BoostProtectionSets {
    pub library_paths: HashSet<String>,
    pub transfer_paths: HashSet<String>,
    pub active_subscribe_hashes: HashSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnedDownloadTaskSummary {
    pub task_id: String,
    pub downloader_id: DownloaderId,
    pub ownership_tag: String,
    pub info_hash: Option<String>,
    pub operation_id: String,
    pub site_id: Option<SiteId>,
    pub plugin_id: Option<String>,
    pub enclosure: String,
    pub verified_bytes: Option<i64>,
    pub boost_state: Option<BoostTaskState>,
    pub retained_bytes: i64,
    pub created_at: i64,
}

impl Store {
    pub fn reset_expired_boost_cleanup_plan(
        &self,
        task_fact_id: &str,
        now_utc: i64,
    ) -> Result<BoostTaskFactRecord, StoreError> {
        validate_non_empty(task_fact_id, "task_fact_id must not be empty")?;
        self.with_writer(Db::Subscribe, |conn| {
            let tx = crate::write_tx(conn)?;
            let existing = fetch_tf_by_id(&tx, task_fact_id)?.ok_or(PluginError::NotFound)?;
            if existing.state != BoostTaskState::CleanupPlanned
                || existing
                    .cleanup_plan_expires_at
                    .is_none_or(|exp| exp >= now_utc)
            {
                return Err(PluginError::Conflict.into());
            }
            tx.execute(
                "UPDATE boost_task_facts
                 SET state = 'retained_only', cleanup_plan_handle = NULL,
                     cleanup_plan_expires_at = NULL, revision = revision + 1, updated_at = ?2
                 WHERE task_fact_id = ?1 AND revision = ?3",
                params![task_fact_id, now_utc, existing.revision],
            )?;
            let updated = fetch_tf_by_id(&tx, task_fact_id)?.ok_or(PluginError::NotFound)?;
            tx.commit()?;
            Ok(updated)
        })
    }

    pub fn get_boost_snapshot_for_plugin(
        &self,
        plugin_id: &str,
        site_ids: &[SiteId],
        downloader_ids: &[DownloaderId],
    ) -> Result<BoostSnapshotFacts, StoreError> {
        validate_non_empty(plugin_id, "plugin_id must not be empty")?;
        let filter_sites: HashSet<SiteId> = site_ids.iter().copied().collect();
        let filter_dls: HashSet<DownloaderId> = downloader_ids.iter().copied().collect();
        self.with_subscribe_read(|db| {
            let tx = crate::read_tx(db.subscribe)?;
            let mut r_stmt = tx.prepare(&format!(
                "SELECT {RES_COLS} FROM boost_reservations
                 WHERE plugin_id = ?1 AND stage IN ('provisional','final','unknown_held')
                 ORDER BY updated_at DESC, reservation_id ASC"
            ))?;
            let reservations = r_stmt
                .query_map(params![plugin_id], map_reservation)?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|r| {
                    (filter_sites.is_empty()
                        || r.site_id.is_some_and(|s| filter_sites.contains(&s)))
                        && (filter_dls.is_empty() || filter_dls.contains(&r.downloader_id))
                })
                .collect::<Vec<_>>();
            drop(r_stmt);

            let mut t_stmt = tx.prepare(&format!(
                "SELECT {TF_COLS} FROM boost_task_facts
                 WHERE plugin_id = ?1 AND state != 'cleaned'
                 ORDER BY updated_at DESC, task_fact_id ASC"
            ))?;
            let tasks = t_stmt
                .query_map(params![plugin_id], map_task_fact)?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|t| {
                    (filter_sites.is_empty()
                        || t.site_id.is_some_and(|s| filter_sites.contains(&s)))
                        && (filter_dls.is_empty() || filter_dls.contains(&t.downloader_id))
                })
                .collect::<Vec<_>>();
            drop(t_stmt);
            tx.commit()?;

            let reserved_bytes: i64 = reservations
                .iter()
                .filter(|r| {
                    matches!(
                        r.stage,
                        BoostReservationStage::Final | BoostReservationStage::UnknownHeld
                    )
                })
                .try_fold(0i64, |acc, r| acc.checked_add(r.reserved_bytes))
                .ok_or(PluginError::QuotaExceeded)?;
            let used_bytes: i64 = tasks
                .iter()
                .try_fold(0i64, |acc, t| acc.checked_add(t.occupied_bytes))
                .ok_or(PluginError::QuotaExceeded)?;
            let retained_file_bytes: i64 = tasks
                .iter()
                .try_fold(0i64, |acc, t| acc.checked_add(t.retained_bytes))
                .ok_or(PluginError::QuotaExceeded)?;

            Ok(BoostSnapshotFacts {
                used_bytes,
                reserved_bytes,
                retained_file_bytes,
                reservations,
                tasks,
            })
        })
    }

    pub fn get_boost_task_fact_by_hash(
        &self,
        plugin_id: &str,
        downloader_id: DownloaderId,
        info_hash: &str,
    ) -> Result<Option<BoostTaskFactRecord>, StoreError> {
        validate_non_empty(plugin_id, "plugin_id must not be empty")?;
        let normalized = info_hash.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return Ok(None);
        }
        self.with_subscribe_read(|db| {
            let tx = crate::read_tx(db.subscribe)?;
            let mut stmt = tx.prepare(&format!(
                "SELECT {TF_COLS} FROM boost_task_facts
                 WHERE plugin_id = ?1 AND downloader_id = ?2
                   AND LOWER(info_hash) = ?3 AND state != 'cleaned'
                 ORDER BY updated_at DESC, task_fact_id ASC LIMIT 1"
            ))?;
            let row = stmt
                .query_row(
                    params![plugin_id, downloader_id.to_string(), normalized],
                    map_task_fact,
                )
                .optional()?;
            drop(stmt);
            tx.commit()?;
            Ok(row)
        })
    }

    pub fn find_boost_task_fact_by_handle(
        &self,
        plugin_id: &str,
        handle: &str,
    ) -> Result<Option<BoostTaskFactRecord>, StoreError> {
        validate_non_empty(plugin_id, "plugin_id must not be empty")?;
        let trimmed = handle.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        let lower = trimmed.to_ascii_lowercase();
        self.with_subscribe_read(|db| {
            let tx = crate::read_tx(db.subscribe)?;
            let mut stmt = tx.prepare(&format!(
                "SELECT {TF_COLS} FROM boost_task_facts
                 WHERE plugin_id = ?1 AND state != 'cleaned'
                   AND (task_fact_id = ?2 OR ownership_tag = ?2 OR LOWER(info_hash) = ?3)
                 ORDER BY updated_at DESC, task_fact_id ASC LIMIT 1"
            ))?;
            let row = stmt
                .query_row(params![plugin_id, trimmed, lower], map_task_fact)
                .optional()?;
            drop(stmt);
            tx.commit()?;
            Ok(row)
        })
    }

    pub fn list_active_boost_task_facts(&self) -> Result<Vec<BoostTaskFactRecord>, StoreError> {
        self.with_subscribe_read(|db| {
            let tx = crate::read_tx(db.subscribe)?;
            let mut stmt = tx.prepare(&format!(
                "SELECT {TF_COLS} FROM boost_task_facts
                 WHERE state != 'cleaned'
                 ORDER BY updated_at DESC, task_fact_id ASC"
            ))?;
            let rows = stmt
                .query_map([], map_task_fact)?
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            tx.commit()?;
            Ok(rows)
        })
    }

    pub fn load_boost_protection_sets(&self) -> Result<BoostProtectionSets, StoreError> {
        self.with_read_snapshot(
            &[Db::Library, Db::Subscribe],
            std::time::Duration::from_secs(2),
            || {
                let mut sets = BoostProtectionSets::default();
                self.with_library_read(|db| {
                    let tx = crate::read_tx(db.library)?;
                    collect_library_protection_paths(&tx, &mut sets)?;
                    tx.commit()?;
                    Ok(())
                })?;

                self.with_subscribe_read(|db| {
                    let tx = crate::read_tx(db.subscribe)?;
                    let mut stmt = tx.prepare(
                        "SELECT info_hash FROM pending_downloads WHERE info_hash IS NOT NULL",
                    )?;
                    for row in stmt.query_map([], |r| r.get::<_, String>(0))? {
                        let hash = row?;
                        let norm = hash.trim().to_ascii_lowercase();
                        if !norm.is_empty() {
                            sets.active_subscribe_hashes.insert(norm);
                        }
                    }
                    drop(stmt);
                    tx.commit()?;
                    Ok(())
                })?;

                Ok(sets)
            },
        )?
    }

    pub fn list_owned_download_tasks(&self) -> Result<Vec<OwnedDownloadTaskSummary>, StoreError> {
        self.with_subscribe_read(|db| {
            let tx = crate::read_tx(db.subscribe)?;
            let mut stmt = tx.prepare(
                "SELECT o.downloader_id, o.ownership_tag, o.info_hash, o.operation_id,
                        o.site_id, o.plugin_id, o.enclosure, o.verified_bytes, o.created_at,
                        f.task_fact_id, f.state, COALESCE(f.retained_bytes, 0)
                 FROM download_ownership o
                 LEFT JOIN boost_task_facts f
                   ON f.downloader_id = o.downloader_id AND f.ownership_tag = o.ownership_tag
                 WHERE o.ownership_state = 'owned' AND o.subscribe_id IS NULL
                   AND (f.state IS NULL OR f.state != 'cleaned')
                 ORDER BY o.created_at DESC, o.ownership_tag ASC",
            )?;
            let mut out = Vec::new();
            for row in stmt.query_map([], |r| {
                let dl_raw: String = r.get(0)?;
                let dl_id = dl_raw
                    .parse::<DownloaderId>()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?;
                let site_raw: Option<String> = r.get(4)?;
                let site_id = site_raw
                    .map(|s| s.parse::<SiteId>())
                    .transpose()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?;
                let state_raw: Option<String> = r.get(10)?;
                let boost_state = state_raw
                    .map(|s| s.parse::<BoostTaskState>())
                    .transpose()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?;
                let tf_id: Option<String> = r.get(9)?;
                let op_id: String = r.get(3)?;
                Ok(OwnedDownloadTaskSummary {
                    task_id: tf_id.unwrap_or_else(|| op_id.clone()),
                    downloader_id: dl_id,
                    ownership_tag: r.get(1)?,
                    info_hash: r.get(2)?,
                    operation_id: op_id,
                    site_id,
                    plugin_id: r.get(5)?,
                    enclosure: r.get(6)?,
                    verified_bytes: r.get(7)?,
                    boost_state,
                    retained_bytes: r.get(11)?,
                    created_at: r.get(8)?,
                })
            })? {
                out.push(row?);
            }
            drop(stmt);
            tx.commit()?;
            Ok(out)
        })
    }

    pub fn list_active_boost_enclosures(
        &self,
        quota_key: &str,
        downloader_id: DownloaderId,
    ) -> Result<HashSet<String>, StoreError> {
        validate_non_empty(quota_key, "quota_key must not be empty")?;
        self.with_subscribe_read(|db| {
            let tx = crate::read_tx(db.subscribe)?;
            let mut stmt = tx.prepare(
                "SELECT o.enclosure
                 FROM boost_task_facts f
                 JOIN download_ownership o
                   ON o.downloader_id = f.downloader_id AND o.ownership_tag = f.ownership_tag
                 WHERE f.quota_key = ?1 AND f.downloader_id = ?2 AND f.state != 'cleaned'
                 UNION
                 SELECT d.enclosure
                 FROM boost_reservations r
                 JOIN download_operations d ON d.operation_id = r.operation_id
                 WHERE r.quota_key = ?1 AND r.downloader_id = ?2
                   AND r.stage IN ('provisional','final','unknown_held')",
            )?;
            let rows = stmt
                .query_map(params![quota_key, downloader_id.to_string()], |r| {
                    r.get::<_, String>(0)
                })?
                .collect::<Result<HashSet<String>, _>>()?;
            drop(stmt);
            tx.commit()?;
            Ok(rows)
        })
    }
}

fn insert_path_with_normalized(set: &mut HashSet<String>, raw: &str) {
    if !raw.trim().is_empty() {
        set.insert(raw.to_string());
        set.insert(crate::path_key::normalize_path_key(raw));
    }
}

fn collect_library_protection_paths(
    tx: &rusqlite::Connection,
    sets: &mut BoostProtectionSets,
) -> Result<(), StoreError> {
    let mut stmt = tx.prepare(
        "SELECT path_key, display_path, source_path, publication_state FROM library_files",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    for row in rows {
        let (path_key, display_path, source_path, pub_state) = row?;
        insert_path_with_normalized(&mut sets.library_paths, &path_key);
        insert_path_with_normalized(&mut sets.library_paths, &display_path);
        if let Some(src) = source_path.as_deref().filter(|s| !s.trim().is_empty()) {
            let canon_src = crate::path_key::canonical_recorded_key(tx, src);
            if pub_state == "pending" {
                insert_path_with_normalized(&mut sets.transfer_paths, src);
                sets.transfer_paths.insert(canon_src.clone());
            }
            insert_path_with_normalized(&mut sets.library_paths, src);
            sets.library_paths.insert(canon_src);
        }
    }
    drop(stmt);

    let mut r_stmt = tx.prepare(
        "SELECT from_path, from_path_key, to_path, to_path_key FROM pending_file_renames
         WHERE phase IN ('planned','disk_applied','library_published')",
    )?;
    for row in r_stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })? {
        let (from_path, from_path_key, to_path, to_path_key) = row?;
        for p in [&from_path, &from_path_key, &to_path, &to_path_key] {
            insert_path_with_normalized(&mut sets.transfer_paths, p);
            insert_path_with_normalized(&mut sets.library_paths, p);
        }
    }
    Ok(())
}
