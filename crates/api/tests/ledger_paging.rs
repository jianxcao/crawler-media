//! Ledger keeps the complete management JSON contract while batching SQL facts.
use api::{ApiState, Store};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use domain::{Confidence, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media_server::{
    provider::QueryTiming,
    query_metrics::{QueryMetrics, QueryMetricsSnapshot},
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use store::test_support::{FixtureFile, publish_fixture_file};
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}
struct PersistedStrmSize;
impl api::file_stat_read::FileStatRead for PersistedStrmSize {
    fn read_nfo(&self, _: &Path) -> Option<library::NfoMeta> {
        None
    }
    fn metadata(&self, path: &Path) -> std::io::Result<std::fs::Metadata> {
        assert_ne!(
            path.extension().and_then(|s| s.to_str()),
            Some("strm"),
            "known STRM size needs no filesystem observation"
        );
        std::fs::metadata(path)
    }
}
fn publish(
    store: &Store,
    path: &Path,
    title: &str,
    mode: Option<&str>,
) -> (MediaId, domain::LedgerId) {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: title.into(),
        year: Some(2024),
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let file = publish_fixture_file(
        store,
        &FixtureFile {
            media_id: media.id,
            path: path.to_string_lossy().into_owned(),
            source_path: Some("/downloads/source.mkv".into()),
            transfer_mode: mode.map(str::to_string),
            resolution: Some("1080p".into()),
            codec: Some("hevc".into()),
            hdr: Some("HDR10".into()),
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: Some(88),
            ..Default::default()
        },
    )
    .unwrap();
    (media.id, file.file_id)
}
async fn measured_ledger(state: ApiState) -> (Vec<Value>, QueryMetricsSnapshot) {
    let timing = QueryTiming::start();
    let metrics = QueryMetrics::new(timing, true);
    let mut request = Request::builder()
        .uri("/api/v1/ledger")
        .header("authorization", "Bearer ledger-test")
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(timing);
    request.extensions_mut().insert(metrics.clone());
    let response = api::router(state).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["ok"], true);
    (body["data"].as_array().unwrap().clone(), metrics.snapshot())
}
async fn ledger(state: ApiState) -> Vec<Value> {
    measured_ledger(state).await.0
}

#[tokio::test]
async fn ledger_rows_preserve_owner_media_transfer_and_size() {
    let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let root = dir.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    let owner = store
        .create_library(
            MediaKind::Movie,
            "Owner",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let plain = root.join("plain.mkv");
    std::fs::write(&plain, b"12345").unwrap();
    let hardlink = root.join("hardlink.mkv");
    std::fs::write(&hardlink, b"1234567").unwrap();
    std::fs::hard_link(&hardlink, root.join("same-bytes.alias")).unwrap();
    let strm = root.join("remote.strm");
    std::fs::write(&strm, b"https://media.invalid/movie.mkv").unwrap();
    let missing = root.join("offline.mkv");
    let (media_id, file_id) = publish(&store, &plain, "Plain", Some("copy"));
    publish(&store, &hardlink, "Hardlink", Some("copy"));
    publish(&store, &strm, "STRM", Some("hardlink"));
    publish(&store, &missing, "Offline", Some("move"));
    store
        .record_scan_seen_stamps(
            "ledger-facts",
            &[
                (plain.to_string_lossy().into_owned(), 100, 999, None),
                (strm.to_string_lossy().into_owned(), 100, 333, None),
            ],
        )
        .unwrap();
    let state = ApiState::new(
        store,
        "ledger-test".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("out"),
    )
    .unwrap()
    .with_file_stat_read(Arc::new(PersistedStrmSize));
    let rows = ledger(state).await;
    assert_eq!(
        rows.len(),
        4,
        "existing endpoint returns the complete ledger"
    );
    let row = rows
        .iter()
        .find(|r| r["id"] == file_id.to_string())
        .unwrap();
    assert_eq!(
        row,
        &json!({
            "id": file_id.to_string(), "media_id": media_id.to_string(), "library_id": owner.id,
            "media_title": "Plain", "media_kind": "movie", "path": plain.to_string_lossy(),
            "source_path": "/downloads/source.mkv", "units": [{"season":-1,"episode":-1}],
            "resolution":"1080p", "codec":"hevc", "hdr":"HDR10", "quality_source":"release",
            "confidence":"high", "filter_score":88, "transfer_mode":"copy", "file_size":999
        })
    );
    let find = |title: &str| rows.iter().find(|r| r["media_title"] == title).unwrap();
    assert_eq!(
        find("Hardlink")["transfer_mode"],
        "hardlink",
        "real link count overrides saved copy mode"
    );
    assert_eq!(find("Hardlink")["file_size"], 7);
    assert_eq!(find("STRM")["transfer_mode"], "strm");
    assert_eq!(find("STRM")["file_size"], 333);
    assert_eq!(find("Offline")["transfer_mode"], "move");
    assert_eq!(find("Offline")["file_size"], 0);
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn ledger_work(files: usize) -> QueryMetricsSnapshot {
    std::fs::create_dir_all("/tmp/crawler-storage-fixes").unwrap();
    let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let root = dir.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "Measured ledger",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    for index in 0..files {
        let path = root.join(format!("{index:03}.mkv"));
        std::fs::write(&path, b"finite ledger fixture").unwrap();
        publish(&store, &path, &format!("Ledger {index:03}"), Some("copy"));
    }
    // Outside the runtime: incidental background workers cannot pollute work.
    let state = ApiState::new(
        store,
        "ledger-test".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("out"),
    )
    .unwrap();
    let (rows, metrics) = runtime().block_on(measured_ledger(state));
    assert_eq!(
        rows.len(),
        files,
        "the complete JSON contract stays unpaged"
    );
    for row in &rows {
        assert_eq!(row["media_kind"], "movie");
        assert_eq!(row["transfer_mode"], "copy");
        assert_eq!(row["file_size"], 21);
        assert_eq!(row.as_object().unwrap().len(), 16);
    }
    assert!(metrics.statements.is_some_and(|count| count > 0));
    assert!(metrics.vm_steps.is_some_and(|count| count > 0));
    metrics
}

#[test]
fn ledger_http_statement_count_is_constant_after_file_growth() {
    let small = ledger_work(10);
    let large = ledger_work(200);
    assert_eq!(
        large.statements, small.statements,
        "actual Store worker SQL must be fixed despite complete output growth: {small:?} vs {large:?}"
    );
    // A complete response scans all files; strict VM O(page) is not asserted.
    // Body length and exact original row shape are checked by ledger_work.
}

#[test]
fn ledger_http_reader_checkouts_stay_bounded_after_file_growth() {
    let checkouts = |metrics: QueryMetricsSnapshot| {
        // Diagnostic JSON avoids making this regression a missing-symbol RED.
        // Current QueryMetrics does not yet forward the worker's checkout count.
        let diagnostic = serde_json::to_value(metrics).unwrap();
        let count = diagnostic["reader_checkouts"]
            .as_u64()
            .expect("actual worker reader_checkouts diagnostics must be present");
        assert!(
            count > 0,
            "zero cannot establish a worker-scoped read bound"
        );
        count
    };
    let small = checkouts(ledger_work(10));
    let large = checkouts(ledger_work(200));
    assert_eq!(small, large, "reader admission must not scale per file");
    assert!(
        large <= 2,
        "one Library snapshot and one App settings snapshot suffice: {large}"
    );
}
