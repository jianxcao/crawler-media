//! Deadline-aware page-only auto-probe admission. Source observation precedes Store work.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use super::{ProbeManager, ProbeUnit, policy, probe};
use store::{LibraryReadError, QueryBudget};

enum PersistOutcome {
    Created(String),
    Duplicate,
    ReadOnly,
}

impl ProbeManager {
    fn query_store<T>(
        &self,
        budget: &QueryBudget,
        work: impl FnOnce(&crate::Store) -> Result<T, LibraryReadError>,
    ) -> Result<T, LibraryReadError> {
        budget.check()?;
        let store = self
            .store
            .try_lock_until(budget.deadline)
            .ok_or(LibraryReadError::Deadline)?;
        budget.check()?;
        store.with_query_budget(budget, work)
    }

    /// Preserve request_probe's page assessment and durable dispatch under one request deadline.
    pub(crate) fn request_probe_query(
        &self,
        row: &domain::FileCoverage,
        budget: &QueryBudget,
        writable: Arc<AtomicBool>,
    ) -> Result<policy::ProbeRequestResult, LibraryReadError> {
        budget.check()?;
        let now_ms = self.clock.lock().now_ms();
        self.assessments.fetch_add(1, Ordering::SeqCst);
        let source_version = crate::fingerprint_job::current_source_version(&row.file.path);
        let need = self.query_store(budget, |store| {
            Ok(policy::assess_probe_need(
                store,
                &row.file,
                policy::ProbeRequestOrigin::Detail,
                now_ms,
                source_version,
            ))
        })?;
        if !matches!(need.retry, policy::ProbeRequestResult::Complete) {
            return Ok(need.retry);
        }
        if need.metadata_valid && !need.intro_missing && !need.outro_missing {
            return Ok(policy::ProbeRequestResult::Complete);
        }
        let unit = ProbeUnit {
            row: row.clone(),
            kind: need.kind,
            force_fingerprint: need.fingerprint_enabled,
            reuse_fingerprint_cache: true,
            overwrite_markers: false,
            reuse_media_info_cache: true,
            marker_refresh_id: None,
            job_id: None,
        };
        if need.metadata_valid
            && need.fingerprint_enabled
            && (need.intro_missing || need.outro_missing)
        {
            return self.query_fingerprint(
                unit,
                need.media_duration_ms,
                need.source_version,
                need.sample_duration_secs,
                budget,
                &writable,
            );
        }
        self.query_metadata(unit, budget, &writable)
    }

    fn query_persist(
        &self,
        unit: &ProbeUnit,
        kind: &str,
        budget: &QueryBudget,
        writable: &Arc<AtomicBool>,
    ) -> Result<PersistOutcome, LibraryReadError> {
        let job_id = uuid::Uuid::new_v4().to_string();
        let ledger_id = unit.row.file.file_id.to_string();
        let scope = format!("ledger:{ledger_id}");
        let spec = crate::store::ProbeJobUnitSpec {
            ledger_id: &ledger_id,
            kind: unit.kind.as_str(),
            force_fingerprint: unit.force_fingerprint,
            reuse_fingerprint_cache: unit.reuse_fingerprint_cache,
            overwrite_markers: unit.overwrite_markers,
            reuse_media_info_cache: unit.reuse_media_info_cache,
        };
        self.query_store(budget, |store| {
            if !writable.load(Ordering::Acquire) {
                tracing::warn!(
                    ledger_id,
                    "Page auto-probe persistence skipped after writer lease loss"
                );
                return Ok(PersistOutcome::ReadOnly);
            }
            store
                .create_probe_job(
                    &job_id,
                    kind,
                    &unit.row.file.media_id.to_string(),
                    super::projected_season_opt(&unit.row),
                    &scope,
                    &[spec],
                )
                .map(|created| {
                    if created {
                        PersistOutcome::Created(job_id)
                    } else {
                        PersistOutcome::Duplicate
                    }
                })
                .map_err(Into::into)
        })
    }

    fn query_metadata(
        &self,
        mut unit: ProbeUnit,
        budget: &QueryBudget,
        writable: &Arc<AtomicBool>,
    ) -> Result<policy::ProbeRequestResult, LibraryReadError> {
        let ledger_id = unit.row.file.file_id.to_string();
        if self.seen.lock().contains(&ledger_id) {
            return Ok(policy::ProbeRequestResult::AlreadyRunning);
        }
        if self
            .last_failure
            .lock()
            .get(&ledger_id)
            .is_some_and(|at| at.elapsed() < Duration::from_secs(60))
        {
            return Ok(policy::ProbeRequestResult::AlreadyRunning);
        }
        self.last_failure.lock().remove(&ledger_id);
        let job_id = match self.query_persist(&unit, "media_probe", budget, writable)? {
            PersistOutcome::Created(job_id) => job_id,
            PersistOutcome::Duplicate => return Ok(policy::ProbeRequestResult::AlreadyRunning),
            PersistOutcome::ReadOnly => return Ok(policy::ProbeRequestResult::Disabled),
        };
        unit.job_id = Some(job_id.clone());
        budget.check()?;
        tracing::info!(job_id, "Page auto-probe durably queued");
        if self.queue_metadata(unit).is_err() {
            tracing::info!(
                ledger_id,
                job_id,
                "Metadata dispatch window full; cancelling page admission"
            );
            self.query_store(budget, |store| {
                if !writable.load(Ordering::Acquire) {
                    tracing::warn!(
                        ledger_id,
                        job_id,
                        "Page auto-probe cancellation skipped after writer lease loss"
                    );
                    return Ok(());
                }
                store
                    .cancel_probe_unit(&job_id, &ledger_id, "dispatch window full")
                    .map(|_| ())
                    .map_err(Into::into)
            })?;
            return Ok(policy::ProbeRequestResult::AlreadyRunning);
        }
        self.seen.lock().insert(ledger_id);
        Ok(policy::ProbeRequestResult::Queued { job_id })
    }

    fn query_fingerprint(
        &self,
        mut unit: ProbeUnit,
        media_duration_ms: Option<i64>,
        source_version: String,
        duration_secs: u32,
        budget: &QueryBudget,
        writable: &Arc<AtomicBool>,
    ) -> Result<policy::ProbeRequestResult, LibraryReadError> {
        let ledger_id = unit.row.file.file_id.to_string();
        // Review L3: 只把「这次准入在跑」登记进 `seen`，登记完立刻放锁。
        // 下面的 `query_persist` 要等进程级 `Store` 锁（长批持锁时可能一直等到
        // 请求截止），占着 `seen` 等锁会把同一文件的下一次准入一起拖住。
        if !self.seen.lock().insert(ledger_id.clone()) {
            return Ok(policy::ProbeRequestResult::AlreadyRunning);
        }
        let persisted = self.query_persist(&unit, "fingerprint_probe", budget, writable);
        let job_id = match persisted {
            Ok(PersistOutcome::Created(job_id)) => job_id,
            Ok(PersistOutcome::Duplicate) => {
                self.forget_seen(&ledger_id);
                return Ok(policy::ProbeRequestResult::AlreadyRunning);
            }
            Ok(PersistOutcome::ReadOnly) => {
                self.forget_seen(&ledger_id);
                return Ok(policy::ProbeRequestResult::Disabled);
            }
            Err(error) => {
                self.forget_seen(&ledger_id);
                return Err(error);
            }
        };
        unit.job_id = Some(job_id.clone());
        if let Err(error) = budget.check() {
            self.forget_seen(&ledger_id);
            return Err(error);
        }
        let work = probe::FingerprintWork {
            unit,
            media_duration_ms,
            source_version,
            duration_secs,
            start_job: true,
        };
        let id = store::ProbeDispatchId {
            job_id: job_id.clone(),
            ledger_id: ledger_id.clone(),
            stage: store::ProbeDispatchStage::Fingerprint,
        };
        if self.fingerprint_tx.try_send((id, work)).is_err() {
            self.forget_seen(&ledger_id);
            tracing::info!(
                job_id,
                "Fingerprint window full; durable unit remains queued"
            );
            self.wake.notify_waiters();
        }
        Ok(policy::ProbeRequestResult::Queued { job_id })
    }

    /// 撤销一次准入的 `seen` 登记（失败路径）。锁只在 remove 这一行里持有。
    fn forget_seen(&self, ledger_id: &str) {
        self.seen.lock().remove(ledger_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{Media, MediaId, MediaKind};
    use parking_lot::Mutex;
    use std::time::Instant;

    #[test]
    fn writer_lease_loss_while_page_probe_waits_leaves_no_ledger_job() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::Store::open(dir.path().join("data")).unwrap();
        let root = dir.path().join("movies");
        std::fs::create_dir_all(&root).unwrap();
        let media_id = MediaId::new();
        store
            .insert_media(&Media {
                id: media_id,
                kind: MediaKind::Movie,
                title: "Probe Movie".into(),
                year: None,
                original_title: None,
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
                aliases: Vec::new(),
            })
            .unwrap();
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
        let file = store::test_support::publish_fixture_file(
            &store,
            &store::test_support::FixtureFile {
                media_id,
                path: path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        let row = store
            .file_coverage_by_path(&path.display().to_string())
            .unwrap()
            .unwrap();
        let store = Arc::new(Mutex::new(store));
        let manager = Arc::new(ProbeManager::new(store.clone()));
        let writable = Arc::new(AtomicBool::new(true));
        let guard = store.lock();
        let probe = manager.clone();
        let flag = writable.clone();
        let worker = std::thread::spawn(move || {
            probe.request_probe_query(
                &row,
                &QueryBudget {
                    deadline: Instant::now() + Duration::from_secs(2),
                    cancelled: Arc::new(AtomicBool::new(false)),
                },
                flag,
            )
        });
        while manager.assessments.load(Ordering::SeqCst) == 0 {
            std::thread::yield_now();
        }
        writable.store(false, Ordering::SeqCst);
        drop(guard);
        let result = worker.join().unwrap().unwrap();
        assert_eq!(result, policy::ProbeRequestResult::Disabled);
        assert!(
            !store
                .lock()
                .probe_unit_recorded_for_ledger(&file.file_id.to_string())
                .unwrap()
        );
    }

    /// 发布一条可探测的台账行，返回它的覆盖事实。
    fn published_row(dir: &std::path::Path) -> (crate::Store, domain::FileCoverage) {
        let store = crate::Store::open(dir.join("data")).unwrap();
        let root = dir.join("movies");
        std::fs::create_dir_all(&root).unwrap();
        let media_id = MediaId::new();
        store
            .insert_media(&Media {
                id: media_id,
                kind: MediaKind::Movie,
                title: "Seen Scope Movie".into(),
                year: None,
                original_title: None,
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
                aliases: Vec::new(),
            })
            .unwrap();
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
        store::test_support::publish_fixture_file(
            &store,
            &store::test_support::FixtureFile {
                media_id,
                path: path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        let row = store
            .file_coverage_by_path(&path.display().to_string())
            .unwrap()
            .unwrap();
        (store, row)
    }

    /// Review L3: `query_fingerprint` 占着 `seen` 去等进程级 `Store` 锁，同一文件的
    /// 下一次准入就会排到这次请求的截止时间之后。`seen` 必须在 `query_persist`
    /// 开始前释放。
    #[test]
    fn seen_is_released_while_persist_waits_for_the_store_lock() {
        let dir = tempfile::tempdir().unwrap();
        let (store, row) = published_row(dir.path());
        let ledger_id = row.file.file_id.to_string();
        let store = Arc::new(Mutex::new(store));
        let manager = Arc::new(ProbeManager::new(store.clone()));
        let guard = store.lock();

        let probe = manager.clone();
        let worker = std::thread::spawn(move || {
            let writable = Arc::new(AtomicBool::new(true));
            probe.query_fingerprint(
                ProbeUnit {
                    row,
                    kind: MediaKind::Movie,
                    force_fingerprint: false,
                    reuse_fingerprint_cache: false,
                    overwrite_markers: false,
                    reuse_media_info_cache: false,
                    marker_refresh_id: None,
                    job_id: None,
                },
                None,
                String::new(),
                180,
                &QueryBudget {
                    deadline: Instant::now() + Duration::from_secs(5),
                    cancelled: Arc::new(AtomicBool::new(false)),
                },
                &writable,
            )
        });

        // 等到这次准入被登记进 `seen`：说明 worker 已经走到 persist，正在等 Store 锁。
        let wait = Instant::now() + Duration::from_secs(2);
        let mut registered = false;
        while Instant::now() < wait {
            if let Some(seen) = manager.seen.try_lock_for(Duration::from_millis(5))
                && seen.contains(&ledger_id)
            {
                registered = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            registered,
            "指纹准入一直占着 `seen`：persist 等 Store 锁期间别的准入拿不到这把锁（L3）"
        );

        // 断言本身：persist 仍在等 Store 锁，`seen` 必须 50 ms 内可拿。
        let started = Instant::now();
        let seen = manager.seen.try_lock_for(Duration::from_millis(50));
        assert!(
            seen.is_some(),
            "`seen` 在 persist 等 Store 锁期间必须可读（waited {:?}）",
            started.elapsed()
        );
        drop(seen);

        drop(guard);
        let result = worker.join().unwrap().unwrap();
        assert!(matches!(
            result,
            policy::ProbeRequestResult::Queued { .. }
        ));
    }
}
