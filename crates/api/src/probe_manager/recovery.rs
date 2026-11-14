use super::ProbeManager;
#[cfg(test)]
use domain::MediaKind;
use std::sync::atomic::Ordering;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn ordinary_recovery_keeps_other_library_units_queued() {
        for kind in ["probe", "marker_refresh"] {
            let temp = tempfile::tempdir().unwrap();
            let store = Arc::new(store::Store::open(temp.path()).unwrap());
            let media = domain::Media {
                id: domain::MediaId::new(),
                kind: MediaKind::Tv,
                title: "Fixture".into(),
                year: None,
                original_title: None,
                aliases: Vec::new(),
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            };
            store.as_ref().insert_media(&media).unwrap();
            let mut libraries = Vec::new();
            let mut ledger_ids = Vec::new();
            for name in ["removed", "retained"] {
                let root = temp.path().join(name);
                libraries.push(
                    store
                        .as_ref()
                        .create_library(
                            media.kind,
                            name,
                            &[root.to_str().unwrap()],
                            "everyone",
                            true,
                            &[],
                        )
                        .unwrap(),
                );
                let mut row = super::super::test_support::coverage(
                    media.id,
                    &root.join("episode.mkv").to_string_lossy(),
                    Some(1),
                    Some(1),
                );
                super::super::test_support::publish_into(&store.as_ref(), &mut row);
                ledger_ids.push(row.file.file_id.to_string());
            }
            let specs: Vec<_> = ledger_ids
                .iter()
                .map(|id| store::ProbeJobUnitSpec {
                    ledger_id: id,
                    kind: "tv",
                    force_fingerprint: false,
                    reuse_fingerprint_cache: false,
                    overwrite_markers: false,
                    reuse_media_info_cache: true,
                })
                .collect();
            store
                .as_ref()
                .create_probe_job(
                    "mixed",
                    kind,
                    &media.id.to_string(),
                    Some(1),
                    "mixed",
                    &specs,
                )
                .unwrap();
            store
                .as_ref()
                .begin_library_removal(&libraries[0].id)
                .unwrap();
            let manager = ProbeManager::new(store.clone()).unwrap();
            let units = store.as_ref().probe_job_units("mixed").unwrap();
            assert_eq!(
                units
                    .iter()
                    .find(|u| u.ledger_id == ledger_ids[0])
                    .unwrap()
                    .status,
                "cancelled"
            );
            {
                assert_eq!(
                    units
                        .iter()
                        .find(|u| u.ledger_id == ledger_ids[1])
                        .unwrap()
                        .status,
                    "queued"
                );
                assert!(manager.is_queued(&ledger_ids[1]));
                assert!(
                    store
                        .as_ref()
                        .get_probe_job("mixed")
                        .unwrap()
                        .unwrap()
                        .is_active()
                );
            }
        }
    }
}

impl ProbeManager {
    pub(crate) fn recover_pending_jobs(&self) {
        if let Err(error) = self.store.reset_running_probe_units() {
            tracing::error!(%error,"恢复探测单元失败");
            self.recovery_deferred.store(true, Ordering::SeqCst);
            return;
        }
        self.retire_missing_source_work();
        self.repair_removed_window();
        self.recover_completed_jobs();
        self.fill_dispatch_windows();
    }
    fn retire_missing_source_work(&self) {
        loop {
            let refs = match self.store.missing_source_probe_unit_refs(64) {
                Ok(refs) => refs,
                Err(error) => {
                    tracing::error!(%error,"读取缺失源任务失败");
                    return;
                }
            };
            if refs.is_empty() {
                return;
            }
            for (job, file) in refs {
                if let Err(error) = self.store.cancel_probe_unit(&job, &file, "file_missing") {
                    tracing::error!(%error,job,file,"取消缺失源单元失败");
                    return;
                }
                self.seen.lock().remove(&file);
            }
        }
    }
    fn repair_removed_window(&self) {
        let refs = match self.store.active_probe_unit_refs(64) {
            Ok(refs) => refs,
            Err(error) => {
                tracing::error!(%error,"读取恢复窗口失败");
                return;
            }
        };
        for (job, file) in refs {
            let row = file
                .parse()
                .ok()
                .and_then(|id| self.store.file_coverage_by_id(id).ok().flatten());
            if row.as_ref().is_none_or(|row| {
                !self
                    .store
                    .library_accepts_work(&row.file.library_id.to_string())
                    .unwrap_or(false)
            }) {
                if let Err(error) = self.store.cancel_probe_unit(&job, &file, "Library removed") {
                    tracing::error!(%error,job,file,"取消删除库参与者失败");
                }
                self.seen.lock().remove(&file);
            }
        }
    }
    pub(super) fn recover_completed_jobs(&self) {
        let jobs = match self.store.active_completed_probe_jobs(64) {
            Ok(jobs) => jobs,
            Err(error) => {
                tracing::error!(%error,"读取已完成探测任务失败");
                return;
            }
        };
        for job in jobs {
            if let Err(error) = self.store.finish_probe_job(
                &job.id,
                job.failed == 0 && job.partial == 0,
                job.error.as_deref(),
            ) {
                tracing::error!(%error,job_id=%job.id,"恢复探测终态失败");
            }
        }
    }
}
