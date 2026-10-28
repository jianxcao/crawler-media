//! chunk 内 Media 身份解析与元数据/刮削落盘。
//!
//! 入账（ledger）先于这里的解析：解析失败时调用方必须把已提交前缀计入失败报告（P3-3）。

use std::collections::HashMap;
use std::path::PathBuf;

use axum::http::StatusCode;
use axum::response::Response;

use crate::http::err;
use crate::management::ApiState;

use super::super::{coverage_by_path, media_by_id, scrape_library_metadata};

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
                let coverages = id
                    .parse()
                    .map(|library_id| scrape_coverages(&store, library_id, &resolved))
                    .unwrap_or_default();
                (coverage, coverages)
            };
            scrape_library_metadata(state, &resolved, &coverage, path, &coverages);
        }
    }

    Ok(())
}

/// Coverage the scrape of one resolved Media consumes: only a TV Media's own episodes.
///
/// A Movie scrape reads none, and a TV scrape seeks its Media instead of hydrating
/// every file of the Library once per scraped Media (review L4).
fn scrape_coverages(
    store: &crate::Store,
    library: domain::LibraryId,
    media: &domain::Media,
) -> Vec<domain::FileCoverage> {
    if media.kind != domain::MediaKind::Tv {
        return Vec::new();
    }
    store
        .file_coverage_in_library(library, Some(media.id), store::FileReadScope::AllRecorded)
        .unwrap_or_default()
}

#[cfg(test)]
mod scrape_coverage_tests {
    use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource, UnitKey};

    use super::scrape_coverages;

    fn media(store: &crate::Store, kind: MediaKind) -> Media {
        let media = Media {
            id: MediaId::new(),
            kind,
            title: "Scope".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&media).unwrap();
        media
    }

    fn file(media: MediaId, path: String, unit: UnitKey) -> store::ScannedLedger {
        store::ScannedLedger {
            file_id: LedgerId::new(),
            media_id: media,
            path,
            units: vec![unit],
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
            source_version: "v1".into(),
            source_digest: None,
            size_bytes: None,
            created_ns: None,
            modified_ns: None,
            artwork: Default::default(),
            observations: vec![],
        }
    }

    /// Review L4: the per-Media scrape reread must not hydrate the whole Library.
    #[test]
    fn scrape_reads_only_the_scraped_tv_media_and_nothing_for_movies() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::Store::open(temp.path()).unwrap();
        let library = store
            .create_library(MediaKind::Tv, "Scope", &["/scope"], "everyone", true, &[])
            .unwrap();
        let target = media(&store, MediaKind::Tv);
        let movie = media(&store, MediaKind::Movie);
        let mut rows = vec![
            file(target.id, "/scope/t/e1.strm".into(), UnitKey::new(1, 1)),
            file(target.id, "/scope/t/e2.strm".into(), UnitKey::new(1, 2)),
        ];
        for index in 0..50 {
            let other = media(&store, MediaKind::Tv);
            rows.push(file(
                other.id,
                format!("/scope/o{index}/e1.strm"),
                UnitKey::new(1, 1),
            ));
        }
        store.insert_scanned_files(&rows, 1000).unwrap();
        let library_id = library.id.parse().unwrap();

        let tv = scrape_coverages(&store, library_id, &target);
        assert_eq!(tv.len(), 2);
        assert!(
            tv.iter()
                .all(|coverage| coverage.file.media_id == target.id)
        );
        assert!(scrape_coverages(&store, library_id, &movie).is_empty());
    }
}
