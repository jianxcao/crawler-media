use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use store::test_support::FixtureFile;
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

fn work_id(media_id: MediaId) -> String {
    domain::TypedItemId::Work(media_id).to_wire_string()
}

fn unit_id(media_id: MediaId, season: i32, episode: i32) -> String {
    domain::TypedItemId::Unit {
        media: media_id,
        key: domain::UnitKey { season, episode },
    }
    .to_wire_string()
}

fn season_item_id(media_id: MediaId, season: u32) -> String {
    domain::TypedItemId::Season {
        media: media_id,
        season,
    }
    .to_wire_string()
}

/// Issue a play session through the documented PlaybackInfo path and return the
/// `PlaySessionId` later session events must carry.
async fn play_session_for(app: &axum::Router, item_id: &str, device: Option<&str>) -> String {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/Items/{item_id}/PlaybackInfo"))
        .header("authorization", "Bearer admin-token")
        .header("content-type", "application/json");
    if let Some(device) = device {
        builder = builder
            .header("X-Emby-Device-Id", device)
            .header("X-Emby-Device-Name", "Bedroom Apple TV")
            .header("X-Emby-Client", "Infuse")
            .header("X-Emby-Client-Version", "8.1");
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from("{}")).unwrap())
        .await
        .unwrap();
    json(response).await["PlaySessionId"]
        .as_str()
        .expect("PlaybackInfo must issue a PlaySessionId")
        .to_string()
}

/// Post one session event without device headers (the request fingerprint is
/// the device identity). Returns the response status.
async fn post_event_without_device(
    app: &axum::Router,
    path: &str,
    item_id: &str,
    play_session_id: &str,
    ticks: i64,
) -> StatusCode {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("authorization", "Bearer admin-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "ItemId": item_id,
                        "PositionTicks": ticks,
                        "PlaySessionId": play_session_id
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    response.status()
}

fn seed_movie_root(store: &Store, root: &Path) {
    if store
        .list_library_roots()
        .unwrap()
        .iter()
        .any(|entry| entry.path == root)
    {
        return;
    }
    store
        .insert_library_root(MediaKind::Movie, root.to_str().unwrap())
        .unwrap();
}

/// Publish one ledger row with the file's real stat facts.
///
/// `publish_fixture_file` never touches the filesystem, so a file published that way keeps
/// `created_ns`/`mtime_ns` NULL and `library_items.added_at_sort` collapses to
/// "unknown". Only the scan commit (`insert_scanned_files`) records stat, which is
/// what `SortBy=DateCreated` orders by.
fn publish_scanned(store: &Store, spec: FixtureFile) {
    let observation = library::observe_source(Path::new(&spec.path)).unwrap();
    let units = match (spec.season, spec.episode) {
        (Some(season), Some(episode)) => vec![domain::UnitKey::new(season as i32, episode as i32)],
        _ => vec![domain::UnitKey::WHOLE],
    };
    store
        .insert_scanned_files(
            &[store::ScannedLedger {
                file_id: spec.file_id.unwrap_or_else(LedgerId::new),
                media_id: spec.media_id,
                path: spec.path,
                units,
                resolution: spec.resolution,
                codec: spec.codec,
                hdr: spec.hdr,
                quality_source: spec.quality_source,
                confidence: spec.confidence,
                filter_score: spec.filter_score,
                source_version: observation.source_version,
                source_digest: observation.content_digest,
                size_bytes: observation
                    .size_bytes
                    .and_then(|size| i64::try_from(size).ok()),
                created_ns: observation.created_ns,
                modified_ns: observation.modified_ns,
                artwork: Default::default(),
                observations: Vec::new(),
            }],
            1_000,
        )
        .unwrap();
}

fn app(root: &Path) -> (axum::Router, MediaId, String) {
    app_seeded(root, |_| {})
}

/// Same as [`app`], but calls `seed` before the ledger row is written: Task 8
/// observes sidecar files only while inserting a ledger row, so a fixture that
/// wants its NFO to reach the page must write it first.
fn app_seeded(root: &Path, seed: impl FnOnce(&Path)) -> (axum::Router, MediaId, String) {
    let store = Store::open(root.join("data")).unwrap();
    seed_movie_root(&store, root);
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("603".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let file = root.join("matrix.mkv");
    std::fs::write(&file, b"0123456789abcdef").unwrap();
    seed(root);
    let spec = store::test_support::FixtureFile {
        media_id: media.id,
        path: file.display().to_string(),
        resolution: Some("2160p".into()),
        codec: Some("hevc".into()),
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: Some(100),
        ..Default::default()
    };
    store::test_support::publish_fixture_file(&store, &spec).unwrap();
    let compact = work_id(media.id);
    let router = router(
        ApiState::new(
            store,
            "admin-token".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(root.join("stage"))),
            root.join("library"),
        )
        .unwrap(),
    );
    (router, media.id, compact)
}

struct FakeClock(std::sync::atomic::AtomicI64);

impl api::playback_events::PlaybackClock for FakeClock {
    fn now_ms(&self) -> i64 {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

fn app_with_clock(
    root: &Path,
    clock: Arc<dyn api::playback_events::PlaybackClock>,
) -> (axum::Router, MediaId, String) {
    let store = Store::open(root.join("data")).unwrap();
    seed_movie_root(&store, root);
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("603".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let file = root.join("matrix.mkv");
    std::fs::write(&file, b"0123456789abcdef").unwrap();
    let spec = store::test_support::FixtureFile {
        media_id: media.id,
        path: file.display().to_string(),
        resolution: Some("2160p".into()),
        codec: Some("hevc".into()),
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: Some(100),
        ..Default::default()
    };
    store::test_support::publish_fixture_file(&store, &spec).unwrap();
    let compact = work_id(media.id);
    let state = ApiState::new(
        store,
        "admin-token".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
    .with_playback_clock(clock);
    (router(state), media.id, compact)
}

async fn json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[path = "jellyfin/authentication_routes.rs"]
mod authentication_routes;
#[path = "jellyfin/endpoint_routes.rs"]
mod endpoint_routes;
#[path = "jellyfin/library_routes.rs"]
mod library_routes;
#[path = "jellyfin/user_marks_routes.rs"]
mod user_marks_routes;

#[path = "jellyfin/catalog.rs"]
mod catalog;
#[path = "jellyfin/playback.rs"]
mod playback;
#[path = "jellyfin/protocol.rs"]
mod protocol;
#[path = "jellyfin/visibility.rs"]
mod visibility;

#[path = "jellyfin/filters.rs"]
mod filters;
#[path = "jellyfin/metadata_parity.rs"]
mod metadata_parity;
#[path = "jellyfin/metadata_scrape.rs"]
mod metadata_scrape;
#[path = "jellyfin/people.rs"]
mod people;
#[path = "jellyfin/versions.rs"]
mod versions;
