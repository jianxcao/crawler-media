//! A12: a failed guarded export preparation must never degrade into an
//! unguarded in-place `write_nfo` of a ledgered video's sidecar.

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

/// A ledgered sidecar the export cannot read (EACCES) keeps its bytes: the
/// preparation error stops this file's export instead of falling back to write_nfo.
#[cfg(unix)]
#[tokio::test]
async fn unreadable_ledgered_sidecar_is_not_rewritten_in_place() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let catalog = Arc::new(ExportCatalog::default());
    catalog.set_overview("V1");
    let state = export_state(temp.path(), catalog.clone());
    let (library_id, video, nfo) = movie_fixture(&state);
    let app = api::router(state.clone());
    common::scan_and_wait(&app, &library_id.to_string()).await;
    let media = media_at(&state, &video);

    let nfo_bytes_before = nfo_body("edited outside, unreadable now").into_bytes();
    std::fs::write(&nfo, &nfo_bytes_before).unwrap();
    // Write-only: reading fails with EACCES while an unguarded write would succeed.
    std::fs::set_permissions(&nfo, std::fs::Permissions::from_mode(0o200)).unwrap();
    struct Restore<'a>(&'a Path);
    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o644));
        }
    }
    let restore = Restore(&nfo);
    if std::fs::read(&nfo).is_ok() {
        eprintln!("skipped: running with privileges that ignore file modes");
        return;
    }
    catalog.set_overview("V2");
    refresh_item(&app, library_id, media).await;
    drop(restore);
    let nfo_bytes_after_failed_prepare = std::fs::read(&nfo).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&nfo_bytes_after_failed_prepare),
        String::from_utf8_lossy(&nfo_bytes_before)
    );
}

/// The guarded path still works once the sidecar is readable again.
#[tokio::test]
async fn readable_ledgered_sidecar_still_exports_through_the_driver() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Arc::new(ExportCatalog::default());
    catalog.set_overview("V1");
    let state = export_state(temp.path(), catalog.clone());
    let (library_id, video, nfo) = movie_fixture(&state);
    let app = api::router(state.clone());
    common::scan_and_wait(&app, &library_id.to_string()).await;
    let media = media_at(&state, &video);
    catalog.set_overview("V2");
    refresh_item(&app, library_id, media).await;
    let body = std::fs::read_to_string(&nfo).unwrap();
    assert!(body.contains("V2"), "{body}");
}
