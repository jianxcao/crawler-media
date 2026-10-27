//! chunk 内 Media 身份解析与元数据/刮削落盘。
//!
//! 入账（ledger）先于这里的解析：解析失败时调用方必须把已提交前缀计入失败报告（P3-3）。

use std::collections::HashMap;
use std::path::PathBuf;

use axum::http::StatusCode;
use axum::response::Response;

use crate::http::err;
use crate::http::library::coverages_in_library;
use crate::management::ApiState;

use super::super::{coverage_by_path, library_by_id, media_by_id, scrape_library_metadata};

/// chunk 内涉及到的非 Video Media 台账覆盖（身份解析按 Media 去重）。
pub(super) fn scan_media_for(
    state: &ApiState,
    files: &[library::TransferredFile],
) -> HashMap<domain::MediaId, (domain::Media, domain::FileCoverage)> {
    let store = state.store.lock();
    let _hold = super::hold::trace_lock_section("scan_media_for");
    let mut map = HashMap::new();
    for file in files {
        let Some(coverage) = coverage_by_path(&store, &file.path.display().to_string()) else {
            continue;
        };
        let Some(m) = media_by_id(&store, coverage.file.media_id) else {
            continue;
        };
        if m.kind != domain::MediaKind::Video {
            map.entry(m.id).or_insert((m, coverage));
        }
    }
    map
}

/// 对 chunk 内已入账的 Media 做身份收敛与刮削（NFO / 图片）。
pub(super) fn resolve_scan_metadata(
    state: &ApiState,
    id: &str,
    lease: &crate::library_lifecycle::LibraryWorkLease,
    scan_media: HashMap<domain::MediaId, (domain::Media, domain::FileCoverage)>,
    inserted: &[PathBuf],
    scrape: bool,
) -> Result<(), Response> {
    for (_, (media, coverage)) in scan_media {
        if lease.control().is_cancelled() {
            return Err(err(
                StatusCode::CONFLICT,
                "library.deleting",
                "Library 正在删除",
            ));
        }
        let path_buf = coverage.file.path.clone();
        let path = path_buf.as_path();
        if crate::auto_resolve::has_local_scan_metadata(state, &media, path) {
            continue;
        }
        let resolved = crate::auto_resolve::resolve_scanned_media(state, &media, path)
            .unwrap_or_else(|| media.clone());
        if let Err(error) =
            crate::auto_resolve::rebind_scan_rows(state, media.id, resolved.id, inserted)
        {
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error,
            ));
        }
        if scrape {
            // 见上：真实失败由 poster_fetch 内部记录，None 不是错误。
            let _ = crate::poster_fetch::attach_poster(state, &resolved, path);
            let _ = crate::poster_fetch::attach_backdrop(state, &resolved, path);
            // Reread after alias convergence; stale coverage still carries the provisional ID.
            let (coverage, coverages) = {
                let store = state.store.lock();
                let _hold = super::hold::trace_lock_section("metadata_reread");
                let key = coverage.file.path.to_string_lossy().into_owned();
                let coverage = coverage_by_path(&store, &key).unwrap_or(coverage);
                let coverages = library_by_id(&store, id)
                    .map(|library| coverages_in_library(&store, &library))
                    .unwrap_or_default();
                (coverage, coverages)
            };
            scrape_library_metadata(state, &resolved, &coverage, path, &coverages);
        }
    }

    Ok(())
}
