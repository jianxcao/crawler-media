//! Explicit media information requests preserve published facts until replacement.
use super::{ProbeManager, ProbeUnit, policy::ProbeRequestResult};

#[derive(Debug, thiserror::Error)]
pub(crate) enum MediaInfoRefreshError {
    #[error("source changed; scan the Library before refreshing media information")]
    SourceChanged,
    #[error(transparent)]
    Store(#[from] store::StoreError),
}

impl ProbeManager {
    pub(crate) fn request_media_info_refresh(
        &self,
        row: &domain::FileCoverage,
        force: bool,
        request_id: &uuid::Uuid,
    ) -> Result<ProbeRequestResult, MediaInfoRefreshError> {
        let ledger_id = row.file.file_id.to_string();
        let identity = format!("media-info:{}:{force}:{ledger_id}", row.file.library_id);
        let job_id = uuid::Uuid::new_v5(request_id, identity.as_bytes()).to_string();
        if let Some(job) = self.store.get_probe_job(&job_id)? {
            // A retried page counts its existing durable jobs even after completion.
            return Ok(recorded_outcome(job));
        }
        if row.file.presence != domain::FilePresence::Present
            || row.file.publication != domain::FilePublication::Published
        {
            return Ok(ProbeRequestResult::Cancelled);
        }
        let source = crate::fingerprint_job::current_source_version(&row.file.path);
        if source != row.file.version.source_version {
            tracing::warn!(file_id = %row.file.file_id, path = %row.file.path.display(),
                "媒体信息抓取源文件已变化，需要先扫描媒体库");
            return Err(MediaInfoRefreshError::SourceChanged);
        }
        if !force {
            let cached = self.store.with_read_snapshot(
                &[store::Db::Library],
                crate::http::store_wait::STORE_READ_WAIT,
                || super::tracks::reusable_media_duration(&self.store, &ledger_id, &source),
            )?;
            if cached.is_some() {
                return Ok(ProbeRequestResult::Complete);
            }
        }
        let mut unit = ProbeUnit {
            row: row.clone(),
            kind: row.file.kind,
            force_fingerprint: false,
            reuse_fingerprint_cache: !force,
            overwrite_markers: false,
            reuse_media_info_cache: !force,
            marker_refresh_id: None,
            job_id: None,
        };
        let created = self.store.library_write(|store| {
            // Recheck database facts after acquiring the actual writer. Every SQL
            // failure propagates; only a changed or unavailable target is skipped.
            if !target_current(store, row)? {
                return Ok(false);
            }
            let created = store.create_probe_job(
                &job_id,
                "media_probe",
                &row.file.media_id.to_string(),
                super::single_season_opt(row),
                &format!("ledger:{ledger_id}"),
                &[store::ProbeJobUnitSpec {
                    ledger_id: &ledger_id,
                    kind: unit.kind.as_str(),
                    force_fingerprint: false,
                    reuse_fingerprint_cache: unit.reuse_fingerprint_cache,
                    overwrite_markers: false,
                    reuse_media_info_cache: unit.reuse_media_info_cache,
                }],
            )?;
            if !created && store.get_probe_job(&job_id)?.is_none() {
                // Keep the skip decision after the covering active work finishes.
                // An isolated, stable scope also permits normal history retention.
                store.record_media_info_refresh_skip(
                    &job_id,
                    &row.file.media_id.to_string(),
                    &format!("media-info-admission:{}:{ledger_id}", row.file.library_id),
                )?;
            }
            Ok(created)
        })?;
        if !created {
            return Ok(self
                .store
                .get_probe_job(&job_id)?
                .map(recorded_outcome)
                .unwrap_or(ProbeRequestResult::AlreadyRunning));
        }
        self.last_failure.lock().remove(&ledger_id);
        unit.job_id = Some(job_id.clone());
        if self.background_is_paused() || self.should_defer_remote_dispatch(&unit.row.file.path) {
            tracing::info!(%job_id, %ledger_id, "后台探测或远程读取已暂停，媒体信息刷新任务保留在持久队列");
        } else if self.try_insert_seen(ledger_id.clone(), job_id.clone())
            && self.queue_metadata(unit).is_err()
        {
            self.remove_seen_if_job(&ledger_id, &job_id);
            tracing::info!(%job_id, %ledger_id, "媒体信息派发窗口已满，抓取任务保留在持久队列");
            self.wake.notify_waiters();
        }
        Ok(ProbeRequestResult::Queued { job_id })
    }
}

fn recorded_outcome(job: store::ProbeJob) -> ProbeRequestResult {
    if job.total == 0 && job.error.as_deref() == Some("media_info_already_running") {
        ProbeRequestResult::AlreadyRunning
    } else {
        ProbeRequestResult::Queued { job_id: job.id }
    }
}

fn target_current(
    store: &store::LibraryConnection<'_>,
    row: &domain::FileCoverage,
) -> Result<bool, store::StoreError> {
    let Some(current) = store.file_coverage_by_id(row.file.file_id)? else {
        return Ok(false);
    };
    let file = &current.file;
    let owner = row.file.library_id.to_string();
    Ok(file.version == row.file.version
        && file.path == row.file.path
        && file.media_id == row.file.media_id
        && file.library_id == row.file.library_id
        && file.root_id == row.file.root_id
        && current.units == row.units
        && file.presence == domain::FilePresence::Present
        && file.publication == domain::FilePublication::Published
        && store.library_accepts_work(&owner)?
        && store.library_lifecycle_state(&owner)? == "ready"
        && store.library_root_owning_key(std::path::Path::new(&current.path_key), file.kind)?
            == Some((owner, file.root_id.clone())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{Media, MediaId, MediaKind};
    use std::sync::Arc;

    #[test]
    fn paused_background_keeps_media_info_refresh_durable_without_buffering_channel() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::Store::open(dir.path().join("data")).unwrap());
        let root = dir.path().join("movies");
        std::fs::create_dir_all(&root).unwrap();
        store
            .create_library(
                MediaKind::Movie,
                "Movies",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let path = root.join("movie.mkv");
        std::fs::write(&path, b"fixture").unwrap();
        let media_id = MediaId::new();
        store
            .insert_media(&Media {
                id: media_id,
                kind: MediaKind::Movie,
                title: "Paused Refresh".into(),
                year: None,
                original_title: None,
                aliases: vec![],
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        let row = super::super::test_support::publish(
            &store,
            &super::super::test_support::coverage(media_id, path.to_str().unwrap(), None, None),
        );
        let manager = ProbeManager::new_paused(store.clone()).unwrap();
        let res = manager
            .request_media_info_refresh(&row, true, &uuid::Uuid::new_v4())
            .unwrap();
        assert!(matches!(res, ProbeRequestResult::Queued { .. }));
        assert!(!manager.holds_dispatch_id(&row.file.file_id.to_string()));
        assert!(manager.take_queued_for_test().is_none());
    }
}
