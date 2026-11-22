//! Paginated `/api/v1/ledger/transfers` contract: SQL filtering, work grouping,
//! STRM zero-local-byte accounting, and untouched legacy `/api/v1/ledger`.

use api::{ApiState, Store};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use domain::{Confidence, LibraryFile, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use store::library_write::probe::{Observed, ProbePublication};
use store::test_support::{FixtureFile, publish_fixture_file};
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}

struct RejectStrmStat;
impl api::file_stat_read::FileStatRead for RejectStrmStat {
    fn read_nfo(&self, _: &Path) -> Option<library::NfoMeta> {
        None
    }

    fn metadata(&self, path: &Path) -> std::io::Result<std::fs::Metadata> {
        assert_ne!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("strm"),
            "transfers endpoint must never stat .strm pointer files: {}",
            path.display()
        );
        std::fs::metadata(path)
    }
}

fn insert_media(store: &Store, kind: MediaKind, title: &str) -> MediaId {
    let media = Media {
        id: MediaId::new(),
        kind,
        title: title.into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    media.id
}

fn publish_item(
    store: &Store,
    media_id: MediaId,
    path: &Path,
    mode: Option<&str>,
    season_ep: Option<(u32, u32)>,
) -> LibraryFile {
    publish_fixture_file(
        store,
        &FixtureFile {
            media_id,
            path: path.to_string_lossy().into_owned(),
            season: season_ep.map(|(s, _)| s),
            episode: season_ep.map(|(_, e)| e),
            source_path: Some("/downloads/origin.mkv".into()),
            transfer_mode: mode.map(str::to_string),
            resolution: Some("2160p".into()),
            codec: Some("hevc".into()),
            hdr: Some("HDR10".into()),
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: Some(90),
            ..Default::default()
        },
    )
    .unwrap()
}

async fn get_json(state: ApiState, uri: &str) -> Value {
    let request = Request::builder()
        .uri(uri)
        .header("authorization", "Bearer transfers-test")
        .body(Body::empty())
        .unwrap();
    let response = api::router(state).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "GET {uri}");
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["ok"], true);
    body["data"].clone()
}

fn build_fixture(dir: &Path) -> (ApiState, MediaId) {
    let store = Store::open(dir.join("data")).unwrap();
    let movie_root = dir.join("movies");
    let tv_root = dir.join("tv/zhifou");
    std::fs::create_dir_all(&movie_root).unwrap();
    std::fs::create_dir_all(&tv_root).unwrap();

    store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[movie_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "TV",
            &[dir.join("tv").to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let hl_movie = insert_media(&store, MediaKind::Movie, "Dune Part Two");
    let hl_path = movie_root.join("Dune.Part.Two.2160p.mkv");
    std::fs::write(&hl_path, b"local-hardlink-video").unwrap();
    std::fs::hard_link(&hl_path, dir.join("dune-origin.link")).unwrap();
    publish_item(&store, hl_movie, &hl_path, Some("hardlink"), None);

    let copy_movie = insert_media(&store, MediaKind::Movie, "Blade Runner 2049");
    let copy_path = movie_root.join("Blade.Runner.2049.2160p.mkv");
    std::fs::write(&copy_path, b"local-copy-video").unwrap();
    publish_item(&store, copy_movie, &copy_path, Some("copy"), None);

    let tv_show = insert_media(&store, MediaKind::Tv, "知否知否应是绿肥红瘦");
    let pointer_120b = format!("https://115.cdn.invalid/stream/{:0<89}", "ep");
    assert_eq!(pointer_120b.len(), 120);

    // Insert out of order (E03, E01, E02) to verify deterministic unit sorting.
    let ep3_path = tv_root.join("S01E03.strm");
    let ep1_path = tv_root.join("S01E01.strm");
    let ep2_path = tv_root.join("S01E02.strm");
    for path in [&ep3_path, &ep1_path, &ep2_path] {
        std::fs::write(path, pointer_120b.as_bytes()).unwrap();
    }
    publish_item(&store, tv_show, &ep3_path, Some("hardlink"), Some((1, 3)));
    let ep1 = publish_item(&store, tv_show, &ep1_path, Some("hardlink"), Some((1, 1)));
    publish_item(&store, tv_show, &ep2_path, Some("hardlink"), Some((1, 2)));

    store
        .record_scan_seen_stamps(
            "transfers-seed",
            &[
                (
                    hl_path.to_string_lossy().into_owned(),
                    100,
                    2_500_000_000,
                    None,
                ),
                (
                    copy_path.to_string_lossy().into_owned(),
                    200,
                    1_500_000_000,
                    None,
                ),
                (ep1_path.to_string_lossy().into_owned(), 300, 120, None),
                (ep2_path.to_string_lossy().into_owned(), 301, 120, None),
            ],
        )
        .unwrap();

    store
        .publish_probe_result(
            ep1.file_id,
            &ProbePublication {
                expected: &ep1.version,
                tracks_version: 1,
                resolution: Some("2160p"),
                codec: Some("hevc"),
                hdr: Observed::Value(Some("HDR10")),
                duration_ms: Some(2_760_000),
                details: None,
            },
            1_000,
        )
        .unwrap();

    let state = ApiState::new(
        store,
        "transfers-test".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.join("stage"))),
    )
    .unwrap();
    (state, tv_show)
}

#[tokio::test]
async fn transfers_summary_excludes_strm_bytes_and_filters_accurately() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let (base_state, _) = build_fixture(dir.path());
    let state = base_state.with_file_stat_read(Arc::new(RejectStrmStat));

    let page1 = get_json(state.clone(), "/api/v1/ledger/transfers?view=files&limit=2").await;
    assert_eq!(page1["view"], "files");
    assert_eq!(page1["limit"], 2);
    assert_eq!(page1["offset"], 0);
    assert_eq!(page1["filtered_total"], 5);
    assert_eq!(page1["filtered_file_total"], 5);
    assert_eq!(page1["has_more"], true);
    assert_eq!(page1["items"].as_array().unwrap().len(), 2);

    let summary = &page1["summary"];
    assert_eq!(summary["total_files"], 5);
    assert_eq!(summary["media_count"], 3);
    assert_eq!(summary["movie_files"], 2);
    assert_eq!(summary["tv_files"], 3);
    assert_eq!(summary["strm_count"], 3);
    assert_eq!(summary["hardlink_count"], 1);
    assert_eq!(summary["copy_count"], 1);
    assert_eq!(summary["move_count"], 0);
    assert_eq!(summary["local_size_bytes"], 4_000_000_000_u64);
    assert_eq!(summary["probed_duration_ms"], 2_760_000);
    assert_eq!(summary["strm_probed_count"], 1);

    let strm_filtered = get_json(
        state.clone(),
        "/api/v1/ledger/transfers?view=files&mode=strm&q=S01E01",
    )
    .await;
    assert_eq!(strm_filtered["filtered_total"], 1);
    assert_eq!(strm_filtered["has_more"], false);
    let strm_item = &strm_filtered["items"][0];
    assert_eq!(strm_item["is_strm"], true);
    assert_eq!(strm_item["transfer_mode"], "strm");
    assert_eq!(strm_item["file_size"], Value::Null);
    assert_eq!(strm_item["duration_ms"], 2_760_000);
    assert_eq!(strm_item["units"], json!([{ "season": 1, "episode": 1 }]));

    // Unstamped S01E03.strm also returns file_size == null without touching disk.
    let ep3_filtered = get_json(
        state,
        "/api/v1/ledger/transfers?view=files&mode=strm&q=S01E03",
    )
    .await;
    assert_eq!(ep3_filtered["items"][0]["file_size"], Value::Null);
    assert_eq!(ep3_filtered["items"][0]["duration_ms"], Value::Null);
}

#[tokio::test]
async fn transfers_groups_aggregates_works_and_legacy_ledger_stays_unpaged() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let (state, tv_show) = build_fixture(dir.path());

    let grouped = get_json(state.clone(), "/api/v1/ledger/transfers?view=groups").await;
    assert_eq!(grouped["view"], "groups");
    assert_eq!(grouped["filtered_total"], 3);
    assert_eq!(grouped["filtered_file_total"], 5);
    assert_eq!(grouped["has_more"], false);

    let groups = grouped["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 3);
    let tv_group = groups
        .iter()
        .find(|g| g["media_id"] == tv_show.to_string())
        .expect("tv group row");
    assert_eq!(tv_group["media_title"], "知否知否应是绿肥红瘦");
    assert_eq!(tv_group["media_kind"], "tv");
    assert_eq!(tv_group["file_count"], 3);
    assert_eq!(tv_group["strm_count"], 3);
    assert_eq!(tv_group["local_size_bytes"], 0);
    assert_eq!(tv_group["total_duration_ms"], 2_760_000);
    assert_eq!(tv_group["primary_mode"], "strm");
    assert_eq!(tv_group["primary_resolution"], "2160p");
    assert!(
        tv_group["sample_dir"]
            .as_str()
            .unwrap()
            .ends_with("tv/zhifou")
    );

    let expanded = get_json(
        state.clone(),
        &format!("/api/v1/ledger/transfers?view=files&media_id={tv_show}&limit=200"),
    )
    .await;
    let episodes: Vec<i64> = expanded["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["units"][0]["episode"].as_i64().unwrap())
        .collect();
    assert_eq!(episodes, vec![1, 2, 3]);

    // Legacy `/api/v1/ledger` stays completely untouched as an unpaged array.
    let legacy = get_json(state, "/api/v1/ledger").await;
    let legacy_rows = legacy.as_array().expect("unpaged array");
    assert_eq!(legacy_rows.len(), 5);
    assert!(
        legacy_rows
            .iter()
            .all(|row| row.as_object().unwrap().len() == 16)
    );
}

#[tokio::test]
async fn transfers_unicode_casefold_search_and_removing_library_exclusion() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let active_root = dir.path().join("active-tv");
    let removing_root = dir.path().join("removing-tv");
    std::fs::create_dir_all(&active_root).unwrap();
    std::fs::create_dir_all(&removing_root).unwrap();

    store
        .create_library(
            MediaKind::Tv,
            "Active TV",
            &[active_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let removing_lib = store
        .create_library(
            MediaKind::Tv,
            "Removing TV",
            &[removing_root.to_str().unwrap()],
            "everyone",
            false,
            &[],
        )
        .unwrap();

    let elite = insert_media(&store, MediaKind::Tv, "Élite");
    let elite_ep = active_root.join("Elite.S01E01.2160p.mkv");
    std::fs::write(&elite_ep, b"elite-bytes").unwrap();
    publish_item(&store, elite, &elite_ep, Some("hardlink"), Some((1, 1)));

    let hidden = insert_media(&store, MediaKind::Tv, "Élite Hidden Copy");
    let hidden_ep = removing_root.join("Hidden.S01E01.2160p.mkv");
    std::fs::write(&hidden_ep, b"hidden-bytes").unwrap();
    publish_item(&store, hidden, &hidden_ep, Some("copy"), Some((1, 1)));

    store
        .record_scan_seen_stamps(
            "unicode-removal-seed",
            &[
                (
                    elite_ep.to_string_lossy().into_owned(),
                    100,
                    1_200_000_000,
                    None,
                ),
                (
                    hidden_ep.to_string_lossy().into_owned(),
                    200,
                    8_800_000_000,
                    None,
                ),
            ],
        )
        .unwrap();
    store.begin_library_removal(&removing_lib.id).unwrap();

    let state = ApiState::new(
        store,
        "transfers-test".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
    )
    .unwrap();

    // Non-ASCII uppercase `"ÉLITE"` (`%C3%89LITE`) matches `"Élite"` via `casefold(...)`,
    // while `removing_lib` is excluded from both summary totals and filtered rows.
    let files_page = get_json(
        state.clone(),
        "/api/v1/ledger/transfers?view=files&q=%C3%89LITE",
    )
    .await;
    assert_eq!(files_page["filtered_total"], 1);
    assert_eq!(files_page["filtered_file_total"], 1);
    assert_eq!(files_page["filtered_group_total"], 1);
    assert_eq!(files_page["items"][0]["media_title"], "Élite");
    assert_eq!(files_page["summary"]["total_files"], 1);
    assert_eq!(files_page["summary"]["media_count"], 1);
    assert_eq!(files_page["summary"]["tv_files"], 1);
    assert_eq!(files_page["summary"]["hardlink_count"], 1);
    assert_eq!(files_page["summary"]["copy_count"], 0);
    assert_eq!(files_page["summary"]["local_size_bytes"], 1_200_000_000_u64);

    let groups_page = get_json(state, "/api/v1/ledger/transfers?view=groups").await;
    assert_eq!(groups_page["filtered_total"], 1);
    let groups = groups_page["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["media_title"], "Élite");
}
