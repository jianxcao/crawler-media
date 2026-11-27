//! Dispatch pending probe obligations bounded at 100 entries per round.

use crate::management::ApiState;

/// Attempt to dispatch up to 100 pending probe obligations.
///
/// Returns the number of dispatched and acknowledged obligations.
pub async fn dispatch_once(state: ApiState) -> Result<usize, String> {
    tokio::task::spawn_blocking(move || dispatch_once_sync(&state))
        .await
        .map_err(|e| e.to_string())?
}

fn dispatch_once_sync(state: &ApiState) -> Result<usize, String> {
    dispatch_obligations(&state.store, &state.probe)
}

const PAGE_LIMIT: usize = 100;
const MAX_PAGES_PER_TICK: usize = 10_000;

pub(crate) fn dispatch_obligations_sync(
    shared: &store::Store,
    probe: &crate::probe_manager::ProbeManager,
) -> Result<usize, String> {
    dispatch_obligations(shared, probe)
}

fn dispatch_obligations(
    shared: &store::Store,
    probe: &crate::probe_manager::ProbeManager,
) -> Result<usize, String> {
    if probe.background_is_paused() {
        return Ok(0);
    }
    let mut acked = 0usize;
    let mut after: Option<(i64, domain::LedgerId)> = None;
    for _ in 0..MAX_PAGES_PER_TICK {
        let obligations = shared
            .scan_probe_obligations_after(after, PAGE_LIMIT)
            .map_err(|e| e.to_string())?;
        if obligations.is_empty() {
            break;
        }
        let page_len = obligations.len();
        let next_cursor = obligations
            .last()
            .map(|last| (last.created_at_ms, last.ledger_id));
        for ob in obligations {
            if dispatch_one_obligation(shared, probe, &ob)? {
                acked += 1;
            }
        }
        if acked >= PAGE_LIMIT || page_len < PAGE_LIMIT || after == next_cursor {
            break;
        }
        after = next_cursor;
    }
    Ok(acked)
}

fn dispatch_one_obligation(
    shared: &store::Store,
    probe: &crate::probe_manager::ProbeManager,
    ob: &store::ScanProbeObligation,
) -> Result<bool, String> {
    // An obligation means "this file still needs a probe". Once presence has recorded the
    // source missing the work cannot succeed, and keeping the row would fill the head of
    // the 100-row dispatch window with files that will never run. Preserve an obligation
    // re-armed by presence after this read.
    let row = match shared
        .file_coverage_by_id(ob.ledger_id)
        .map_err(|error| error.to_string())?
    {
        Some(row)
            if !shared
                .ledger_is_missing(&ob.ledger_id.to_string())
                .map_err(|error| error.to_string())? =>
        {
            row
        }
        _ => {
            return match shared.discard_scan_probe_obligation_if_source_unavailable(ob) {
                Ok(discarded) => Ok(discarded),
                Err(error) => {
                    tracing::warn!(%error, ledger_id = %ob.ledger_id, "清理不可用源文件的探测义务失败");
                    Ok(false)
                }
            };
        }
    };
    let result = probe.request_probe(
        &row,
        crate::probe_manager::policy::ProbeRequestOrigin::Detail,
    );
    acknowledge_probe_request_result(shared, ob, &row, result)
}

fn acknowledge_probe_request_result(
    store: &store::Store,
    ob: &store::ScanProbeObligation,
    row: &domain::FileCoverage,
    result: Result<crate::probe_manager::policy::ProbeRequestResult, store::StoreError>,
) -> Result<bool, String> {
    use crate::probe_manager::policy::ProbeRequestResult;
    match result {
        Ok(ProbeRequestResult::Queued { job_id }) => store
            .ack_scan_probe_obligation_for_work(ob, Some(&job_id))
            .map_err(|error| error.to_string()),
        Ok(ProbeRequestResult::AlreadyRunning) => store
            .ack_scan_probe_obligation_for_work(ob, None)
            .map_err(|error| error.to_string()),
        Ok(
            ProbeRequestResult::Complete
            | ProbeRequestResult::Exhausted
            | ProbeRequestResult::Disabled,
        ) => store
            .ack_scan_probe_obligation_if_current(ob)
            .map_err(|error| error.to_string()),
        Ok(ProbeRequestResult::Cancelled) => {
            match store.discard_scan_probe_obligation_if_source_unavailable(ob) {
                Ok(discarded) => Ok(discarded),
                Err(error) => {
                    tracing::warn!(%error, ledger_id = %ob.ledger_id, "清理已取消探测义务失败");
                    Ok(false)
                }
            }
        }
        Ok(ProbeRequestResult::Waiting { .. }) => Ok(false),
        Err(error) => {
            tracing::error!(%error, ledger_id = %row.file.file_id, "扫描探测义务派发失败，保留义务等待重试");
            Ok(false)
        }
    }
}

/// Start background dispatcher task checking obligations every 5 seconds.
pub fn start_dispatcher(state: ApiState) {
    let shared = std::sync::Arc::downgrade(&state.store);
    let probe = std::sync::Arc::downgrade(&state.probe);
    let instance = std::sync::Arc::downgrade(&state.instance_writable);
    let wake = state.probe.obligation_wake();
    tokio::spawn(async move {
        while let Some(writable) = instance.upgrade() {
            let (Some(shared), Some(probe)) = (shared.upgrade(), probe.upgrade()) else {
                break;
            };
            if writable.load(std::sync::atomic::Ordering::SeqCst) && !probe.background_is_paused() {
                let result =
                    tokio::task::spawn_blocking(move || dispatch_obligations(&shared, &probe))
                        .await;
                match result {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => tracing::error!(%error,"Probe obligation dispatcher failed"),
                    Err(error) => {
                        tracing::error!(%error,"Probe obligation dispatcher worker failed")
                    }
                }
            }
            // The sleep retains Weak handles only; dropping an ApiState can stop
            // its drivers and release its per-instance blocking budgets.
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                _ = wake.notified() => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource, UnitKey};
    use downloader::MemoryDownloader;
    use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
    use std::sync::Arc;
    use store::ScannedLedger;

    struct NoFetch;
    impl Fetcher for NoFetch {
        fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
            Err(IndexerError::Fetch("offline".into()))
        }
    }

    fn insert_pin_file(
        store: &crate::Store,
        media_id: MediaId,
        path: &std::path::Path,
        now_ms: i64,
    ) -> LedgerId {
        std::fs::write(path, b"bytes").unwrap();
        let file_id = LedgerId::new();
        let version = crate::fingerprint_job::current_source_version(path);
        store
            .insert_scanned_files(
                &[ScannedLedger {
                    file_id,
                    media_id,
                    path: path.display().to_string(),
                    units: vec![UnitKey::WHOLE],
                    resolution: None,
                    codec: None,
                    hdr: None,
                    quality_source: QualitySource::Release,
                    confidence: Confidence::High,
                    filter_score: None,
                    source_version: version,
                    source_digest: None,
                    size_bytes: None,
                    created_ns: None,
                    modified_ns: None,
                    artwork: Default::default(),
                    observations: Vec::new(),
                }],
                now_ms,
            )
            .unwrap();
        file_id
    }

    #[test]
    fn cancelled_and_disabled_obligations_do_not_pin() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::Store::open(dir.path().join("data")).unwrap();
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
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: "Pin Test".into(),
            year: None,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&media).unwrap();
        let file_id = insert_pin_file(&store, media.id, &root.join("pin.mkv"), 1);
        let state = ApiState::new(
            store,
            "secret".into(),
            RegistrySnapshot::builtin().unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        )
        .unwrap();
        let store = state.store();
        let ob = store.scan_probe_obligations(10).unwrap().remove(0);
        let row = store.file_coverage_by_id(file_id).unwrap().unwrap();
        let disabled_acked = acknowledge_probe_request_result(
            &store,
            &ob,
            &row,
            Ok(crate::probe_manager::policy::ProbeRequestResult::Disabled),
        )
        .unwrap();
        assert!(disabled_acked, "Disabled obligation must be acked");
        assert!(store.scan_probe_obligations(10).unwrap().is_empty());

        let file_id2 = insert_pin_file(&store, media.id, &root.join("pin2.mkv"), 2);
        rusqlite::Connection::open(dir.path().join("data/library.db"))
            .unwrap()
            .execute(
                "UPDATE library_files SET missing_at = 200 WHERE file_id = ?1",
                [file_id2.to_string()],
            )
            .unwrap();
        let ob2 = store.scan_probe_obligations(10).unwrap().remove(0);
        let row2 = store.file_coverage_by_id(file_id2).unwrap().unwrap();
        let cancelled_acked = acknowledge_probe_request_result(
            &store,
            &ob2,
            &row2,
            Ok(crate::probe_manager::policy::ProbeRequestResult::Cancelled),
        )
        .unwrap();
        assert!(cancelled_acked, "Cancelled with missing_at must discard");
        assert!(store.scan_probe_obligations(10).unwrap().is_empty());
    }
}
