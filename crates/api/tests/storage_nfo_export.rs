//! Plan Task 9 — a generated NFO mirrors effective metadata; it never becomes
//! the authority, and an interrupted export must be recoverable by digest.
//!
//! RED target (must fail before the export driver exists):
//! `generated_nfo_does_not_lock_provider_and_crash_recovery_preserves_edits`.

#[path = "management/common.rs"]
mod common;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use api::ApiState;
use domain::{EntityScope, FieldValue, LibraryId, Media, MediaId, MediaKind, MetadataFields};
use parking_lot::Mutex;
use serde_json::json;
use tower::ServiceExt;

const VIDEO: &str = "Fixture.Movie.2020.strm";
const LOCALE: &str = "zh-CN";

/// Provider catalog whose single movie overview can be flipped between scans.
#[derive(Default)]
struct ExportCatalog {
    overview: Mutex<String>,
    title: Mutex<Option<String>>,
}

impl ExportCatalog {
    fn set_title(&self, title: &str) {
        *self.title.lock() = Some(title.to_string());
    }

    fn title(&self) -> String {
        self.title
            .lock()
            .clone()
            .unwrap_or_else(|| "Fixture Movie".into())
    }

    fn set_overview(&self, overview: &str) {
        *self.overview.lock() = overview.to_string();
    }

    fn hit() -> Media {
        Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: "Fixture Movie".into(),
            year: Some(2020),
            original_title: Some("Fixture Movie".into()),
            aliases: Vec::new(),
            tmdb_id: Some("550".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        }
    }
}

impl api::catalog::Catalog for ExportCatalog {
    fn search_movie(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![media::CatalogHit {
            media: Self::hit(),
            poster_path: None,
            backdrop_path: None,
            rating: None,
            overview: None,
        }])
    }

    fn search_tv(&self, query: &str) -> Result<Vec<media::CatalogHit>, String> {
        self.search_movie(query)
    }

    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }

    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }

    fn details(&self, _kind: MediaKind, _id: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }

    fn metadata_with_preferences(
        &self,
        _kind: MediaKind,
        _id: &str,
        _lang_pref: &[String],
        _country_pref: &[String],
    ) -> Result<Option<media::ItemMeta>, String> {
        Ok(Some(item_meta(&self.overview.lock())))
    }

    /// Production adoption reads the catalog's normalized snapshot of the full response.
    fn normalized_metadata(
        &self,
        _kind: MediaKind,
        _id: &str,
        scope: domain::EntityScope,
        preferences: &domain::MetadataPreferences,
    ) -> Result<Option<domain::NormalizedMetadata>, String> {
        let body = serde_json::json!({
            "title": self.title(), "original_title": "Fixture Movie",
            "overview": *self.overview.lock(), "release_date": "2020-05-01",
            "runtime": 120, "vote_average": 8.5, "vote_count": 100,
            "original_language": "en", "status": "Released",
            "genres": [{"id": 18, "name": "Drama"}],
            "production_companies": [{"name": "Fixture Studio"}],
            "production_countries": [{"iso_3166_1": "US", "name": "United States"}],
        });
        media::normalize_metadata("tmdb", scope, &body.to_string(), preferences)
            .map(Some)
            .map_err(|error| error.to_string())
    }
}

fn item_meta(overview: &str) -> media::ItemMeta {
    media::ItemMeta {
        overview: Some(overview.to_string()),
        rating: Some("8.5".into()),
        runtime_minutes: Some("120".into()),
        tagline: None,
        release_date: Some("2020-05-01".into()),
        last_air_date: None,
        content_rating: None,
        vote_count: Some(100),
        origin_countries: vec!["US".into()],
        original_language: Some("en".into()),
        studios: vec!["Fixture Studio".into()],
        studio_logo: None,
        status: Some("Released".into()),
        number_of_seasons: None,
        number_of_episodes: None,
        directors: Vec::new(),
        creators: Vec::new(),
        genres: vec!["Drama".into()],
        genre_ids: vec![18],
        episode_run_time: Vec::new(),
        cast: Vec::new(),
    }
}

fn nfo_body(plot: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<movie>\n  <title>Fixture Movie</title>\n  <plot>{plot}</plot>\n</movie>\n"
    )
}

fn fields_with_overview(plot: &str) -> MetadataFields {
    let mut fields = MetadataFields::default();
    fields.overview = FieldValue::Value(plot.to_string());
    fields
}

/// A user edit changes only the `<plot>` body; every other byte stays.
fn edit_plot(body: &str, value: &str) -> String {
    let open = body.find("<plot>").expect("plot element") + "<plot>".len();
    let close = open + body[open..].find("</plot>").expect("plot terminator");
    format!("{}{}{}", &body[..open], value, &body[close..])
}

/// The default movie library seeded by `common::state`, plus its video + sidecar.
fn movie_fixture(state: &ApiState) -> (LibraryId, PathBuf, PathBuf) {
    let libraries = state.store().lock().list_libraries().unwrap();
    let library = libraries
        .into_iter()
        .find(|library| library.kind == MediaKind::Movie)
        .expect("default movie library");
    let root = library.root_paths[0].clone();
    std::fs::create_dir_all(&root).unwrap();
    let video = root.join(VIDEO);
    std::fs::write(&video, "https://invalid.example/fixture").unwrap();
    let nfo = video.with_extension("nfo");
    (library.id.parse().unwrap(), video, nfo)
}

fn media_at(state: &ApiState, path: &Path) -> MediaId {
    state
        .store()
        .lock()
        .file_coverage_by_path(&path.display().to_string())
        .unwrap()
        .expect("ledger row for the scanned video")
        .file
        .media_id
}

async fn refresh_item(app: &axum::Router, library: LibraryId, media: MediaId) {
    let request = common::request(
        "POST",
        &format!("/api/v1/libraries/{library}/items/{media}/metadata/refresh"),
        Some("management-secret"),
        json!({}),
    );
    let response = app.clone().oneshot(request).await.unwrap();
    assert!(
        response.status().is_success(),
        "single-item metadata refresh must succeed, got {}",
        response.status()
    );
}

fn export_request(
    state: &ApiState,
    library: LibraryId,
    media: MediaId,
    video: &Path,
    nfo: &Path,
    operation_id: &str,
    export_id: &str,
    plot: &str,
) -> store::metadata::NfoExportRequest {
    let store = state.store();
    let store = store.lock();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|root| root.library_id.as_deref() == Some(library.to_string().as_str()))
        .expect("library root")
        .id;
    let (file_id, expected_version) = store
        .file_version_for_path(&video.display().to_string())
        .unwrap()
        .expect("file version for the scanned video");
    let config_revision = store.library_config_revision(&library.to_string()).unwrap();
    let body = nfo_body(plot);
    store::metadata::NfoExportRequest {
        operation_id: operation_id.to_string(),
        export_id: export_id.to_string(),
        library_id: library,
        media_id: media,
        root_id,
        file_id,
        expected_version,
        config_revision,
        scope: EntityScope::Work,
        locale: LOCALE.to_string(),
        path: nfo.to_path_buf(),
        expected_digest: library::nfo::nfo_digest(nfo).unwrap(),
        output_digest: library::nfo::nfo_body_digest(&body),
        body,
        fields: fields_with_overview(plot),
        field_origins: BTreeMap::new(),
    }
}

#[tokio::test]
async fn generated_nfo_does_not_lock_provider_and_crash_recovery_preserves_edits() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Arc::new(ExportCatalog::default());
    catalog.set_overview("V1");
    let state = common::state(
        temp.path(),
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
    )
    .with_catalog(catalog.clone());
    let shared = state.store();
    let (library_id, video, nfo) = movie_fixture(&state);
    let app = api::router(state.clone());

    // A. the first scan exports the provider value into the sidecar.
    common::scan_and_wait(&app, &library_id.to_string()).await;
    let media = media_at(&state, &video);
    let first = std::fs::read_to_string(&nfo).unwrap();
    assert!(
        first.contains("V1"),
        "the first scan must export the provider overview: {first}"
    );

    // B. the provider moves on while the sidecar on disk is left untouched.
    catalog.set_overview("V2");
    common::scan_and_wait(&app, &library_id.to_string()).await;
    let unchanged_after_v2 = shared
        .lock()
        .library_item_detail(library_id, media, LOCALE)
        .unwrap()
        .unwrap();

    // C. the user edits only the plot, then re-runs a single-item refresh.
    let edited = edit_plot(&std::fs::read_to_string(&nfo).unwrap(), "X");
    std::fs::write(&nfo, edited).unwrap();
    refresh_item(&app, library_id, media).await;
    let edited_after_v2 = shared
        .lock()
        .library_item_detail(library_id, media, LOCALE)
        .unwrap()
        .unwrap();

    assert_eq!(
        unchanged_after_v2
            .metadata
            .overview
            .value()
            .map(String::as_str),
        Some("V2")
    );
    assert_eq!(
        edited_after_v2
            .metadata
            .overview
            .value()
            .map(String::as_str),
        Some("X")
    );

    // D1. crash boundary 1: the intent is durable, the replacement never ran.
    let boundary_a = export_request(
        &state,
        library_id,
        media,
        &video,
        &nfo,
        "op-export-a",
        "export-a",
        "A",
    );
    shared.lock().begin_nfo_export(&boundary_a, 1_000).unwrap();
    api::storage_operations::resume_library_reconfigurations(&state).unwrap();
    assert!(
        std::fs::read_to_string(&nfo).unwrap().contains("A"),
        "recovery must retry an interrupted atomic replacement"
    );

    // D2. crash boundary 2: the replacement ran, the registration never did.
    let intended_export_id = "export-b".to_string();
    let boundary_b = export_request(
        &state,
        library_id,
        media,
        &video,
        &nfo,
        "op-export-b",
        &intended_export_id,
        "B",
    );
    shared.lock().begin_nfo_export(&boundary_b, 2_000).unwrap();
    library::nfo::replace_nfo_atomically(&nfo, &boundary_b.body).unwrap();
    let replaced = std::fs::read_to_string(&nfo).unwrap();
    api::storage_operations::resume_library_reconfigurations(&state).unwrap();
    assert_eq!(
        std::fs::read_to_string(&nfo).unwrap(),
        replaced,
        "recovery must register an already replaced sidecar without rewriting it"
    );
    let registration = shared
        .lock()
        .nfo_export_registration("op-export-b")
        .unwrap()
        .expect("registration after recovery");
    let restarted_export_id = registration.export_id;
    let registered_export_digest = registration.output_digest;
    let disk_digest = library::nfo::nfo_digest(&nfo).unwrap().unwrap();

    assert_eq!(restarted_export_id, intended_export_id);
    assert_eq!(disk_digest, registered_export_digest);
}

/// A scan that exports, then an export intent left behind while the user writes
/// to the sidecar: recovery must keep the user's text, not overwrite it.
#[tokio::test]
async fn crash_recovery_escalates_an_external_edit_instead_of_discarding_it() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Arc::new(ExportCatalog::default());
    catalog.set_overview("V1");
    let state = common::state(
        temp.path(),
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
    )
    .with_catalog(catalog.clone());
    let shared = state.store();
    let (library_id, video, nfo) = movie_fixture(&state);
    let app = api::router(state.clone());
    common::scan_and_wait(&app, &library_id.to_string()).await;
    let media = media_at(&state, &video);

    // The intent is durable, then the sidecar is edited before the replacement
    // ever runs: neither the seen digest nor the intended body matches on disk.
    let pending = export_request(
        &state,
        library_id,
        media,
        &video,
        &nfo,
        "op-export-c",
        "export-c",
        "C",
    );
    shared.lock().begin_nfo_export(&pending, 1_000).unwrap();
    let external = edit_plot(&std::fs::read_to_string(&nfo).unwrap(), "KEEPME");
    std::fs::write(&nfo, &external).unwrap();

    api::storage_operations::resume_library_reconfigurations(&state).unwrap();

    let body = std::fs::read_to_string(&nfo).unwrap();
    assert!(
        body.contains("KEEPME"),
        "an edit made during the crash window must survive recovery: {body}"
    );
    let after = shared
        .lock()
        .library_item_detail(library_id, media, LOCALE)
        .unwrap()
        .unwrap();
    assert_eq!(
        after.metadata.overview.value().map(String::as_str),
        Some("KEEPME"),
        "the external edit must be escalated into user overrides"
    );
}

/// When the sidecar cannot be parsed, recovery refuses to touch it: the file
/// stays byte-identical and the export stays unfinished for the next retry.
#[tokio::test]
async fn crash_recovery_refuses_to_overwrite_an_unreadable_sidecar() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Arc::new(ExportCatalog::default());
    catalog.set_overview("V1");
    let state = common::state(
        temp.path(),
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
    )
    .with_catalog(catalog.clone());
    let shared = state.store();
    let (library_id, video, nfo) = movie_fixture(&state);
    let app = api::router(state.clone());
    common::scan_and_wait(&app, &library_id.to_string()).await;
    let media = media_at(&state, &video);

    let pending = export_request(
        &state,
        library_id,
        media,
        &video,
        &nfo,
        "op-export-d",
        "export-d",
        "D",
    );
    shared.lock().begin_nfo_export(&pending, 1_000).unwrap();
    let garbage = "<movie><plot>half written".to_string();
    std::fs::write(&nfo, &garbage).unwrap();

    api::storage_operations::resume_library_reconfigurations(&state).unwrap();

    assert_eq!(
        std::fs::read_to_string(&nfo).unwrap(),
        garbage,
        "an unreadable sidecar must be left exactly as it was found"
    );
    assert!(
        shared
            .lock()
            .nfo_export_registration("op-export-d")
            .unwrap()
            .is_none(),
        "a refused export must not register"
    );
}

fn export_state(temp: &Path, catalog: Arc<ExportCatalog>) -> ApiState {
    common::state(
        temp,
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(downloader::MemoryDownloader::new(temp.join("stage"))),
    )
    .with_catalog(catalog)
}

/// R17: a normal single-item refresh must not overwrite a sidecar it cannot read.
#[tokio::test]
async fn normal_refresh_preserves_invalid_external_sidecar() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Arc::new(ExportCatalog::default());
    catalog.set_overview("V1");
    let state = export_state(temp.path(), catalog.clone());
    let (library_id, video, nfo) = movie_fixture(&state);
    let app = api::router(state.clone());
    common::scan_and_wait(&app, &library_id.to_string()).await;
    let media = media_at(&state, &video);

    let disk_before = "<movie><plot>half written".to_string();
    std::fs::write(&nfo, &disk_before).unwrap();
    catalog.set_overview("V2");
    refresh_item(&app, library_id, media).await;
    let disk_after_refused = std::fs::read_to_string(&nfo).unwrap();
    assert_eq!(disk_after_refused, disk_before);
}

/// R18: an edit to the plot alone must not lock the title the provider later changes.
#[tokio::test]
async fn provider_title_changes_when_only_plot_was_edited() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Arc::new(ExportCatalog::default());
    catalog.set_overview("PLOT V1");
    catalog.set_title("TITLE V1");
    let state = export_state(temp.path(), catalog.clone());
    let (library_id, video, nfo) = movie_fixture(&state);
    let app = api::router(state.clone());
    common::scan_and_wait(&app, &library_id.to_string()).await;
    let media = media_at(&state, &video);
    assert!(std::fs::read_to_string(&nfo).unwrap().contains("TITLE V1"));

    catalog.set_title("TITLE V2");
    let edited = edit_plot(&std::fs::read_to_string(&nfo).unwrap(), "USER PLOT");
    std::fs::write(&nfo, edited).unwrap();
    refresh_item(&app, library_id, media).await;
    let after = state
        .store()
        .lock()
        .library_item_detail(library_id, media, LOCALE)
        .unwrap()
        .unwrap()
        .metadata;
    assert_eq!(after.title.value().map(String::as_str), Some("TITLE V2"));
    assert_eq!(
        after.overview.value().map(String::as_str),
        Some("USER PLOT")
    );
}

/// S01: an export holds the Store mutex only for database phases. Every lock
/// acquisition records whether the sidecar exists yet; the file must appear
/// between two guards, never while one is alive.
#[tokio::test]
async fn phased_export_writes_the_sidecar_with_the_store_unlocked() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Tracked<'a> {
        guard: parking_lot::MutexGuard<'a, store::Store>,
        live: &'a AtomicUsize,
    }
    impl std::ops::Deref for Tracked<'_> {
        type Target = store::Store;
        fn deref(&self) -> &store::Store {
            &self.guard
        }
    }
    impl Drop for Tracked<'_> {
        fn drop(&mut self) {
            self.live.fetch_sub(1, Ordering::SeqCst);
        }
    }

    let temp = tempfile::tempdir().unwrap();
    let catalog = Arc::new(ExportCatalog::default());
    catalog.set_overview("V1");
    let state = export_state(temp.path(), catalog);
    let (library_id, video, nfo) = movie_fixture(&state);
    let app = api::router(state.clone());
    common::scan_and_wait(&app, &library_id.to_string()).await;
    let media = media_at(&state, &video);
    std::fs::remove_file(&nfo).unwrap();
    let request = export_request(
        &state,
        library_id,
        media,
        &video,
        &nfo,
        "op-phased",
        "export-phased",
        "P",
    );

    let shared = state.store();
    let live = AtomicUsize::new(0);
    let seen = Mutex::new(Vec::new());
    let lock = || {
        assert_eq!(
            live.fetch_add(1, Ordering::SeqCst),
            0,
            "phases must not nest"
        );
        seen.lock().push(nfo.exists());
        Tracked {
            guard: shared.lock(),
            live: &live,
        }
    };
    let outcome =
        store::metadata::export_driver::export_nfo_sidecar_phased(lock, &request, 5_000).unwrap();
    assert_eq!(
        outcome.action,
        store::metadata::export_driver::NfoExportAction::Replaced
    );
    let seen = seen.into_inner();
    assert!(seen.len() >= 3, "plan, registration plan, finish: {seen:?}");
    assert!(!seen[0], "the plan phase runs before the write");
    assert!(
        seen[1..].iter().all(|exists| *exists),
        "the write happened between guards: {seen:?}"
    );
    assert_eq!(live.load(Ordering::SeqCst), 0);
    assert!(
        shared
            .lock()
            .nfo_export_registration("op-phased")
            .unwrap()
            .is_some(),
        "the final locked phase registers the export"
    );
}
