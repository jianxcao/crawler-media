#[path = "../support/mod.rs"]
mod shared;

use api::catalog::Catalog;
use domain::{LibraryFile, Media, MediaId, MediaKind};
use parking_lot::Mutex;
use serde_json::Value;
use shared::library_presence::PresenceFixture;
use std::sync::Arc;

pub fn hit(id: &str, kind: MediaKind, year: Option<u16>) -> media::CatalogHit {
    media::CatalogHit {
        media: Media {
            id: MediaId::new(),
            kind,
            title: "Twin Story".into(),
            year,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some(id.into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        },
        poster_path: None,
        backdrop_path: None,
        rating: None,
        overview: None,
    }
}

pub struct FakeCatalog {
    pub result: Mutex<Result<Vec<media::CatalogHit>, String>>,
}

impl Catalog for FakeCatalog {
    fn search_movie(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        self.result.lock().clone()
    }
    fn search_tv(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        self.result.lock().clone()
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(&self, kind: MediaKind, id: &str) -> Result<Option<Media>, String> {
        self.result.lock().clone().map(|hits| {
            hits.into_iter()
                .find(|hit| hit.media.kind == kind && hit.media.tmdb_id.as_deref() == Some(id))
                .map(|hit| hit.media)
        })
    }
}

pub struct Fixture {
    pub fix: PresenceFixture,
    pub file: LibraryFile,
    pub media: Media,
    pub catalog: Arc<FakeCatalog>,
}

impl Fixture {
    pub fn new(result: Result<Vec<media::CatalogHit>, String>) -> Self {
        let mut fix = PresenceFixture::new();
        let catalog = Arc::new(FakeCatalog {
            result: Mutex::new(result),
        });
        fix.state = fix.state.with_catalog(catalog.clone());
        let mut media = hit("unused", MediaKind::Movie, Some(2024)).media;
        media.tmdb_id = None;
        let path = fix.root.join("Twin.Story.2024.strm");
        std::fs::write(&path, "https://example.invalid/film.mkv").unwrap();
        let store = fix.state.store();
        store.insert_media(&media).unwrap();
        let file = store::test_support::publish_fixture_file(
            &store,
            &store::test_support::FixtureFile {
                media_id: media.id,
                path: path.to_string_lossy().into_owned(),
                ..Default::default()
            },
        )
        .unwrap();
        Self {
            fix,
            file,
            media,
            catalog,
        }
    }

    pub fn preview(&self, media: MediaId) -> Value {
        let uri = format!(
            "/api/v1/libraries/{}/items/{media}/reidentification-preview",
            self.fix.library_id
        );
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let (status, body) = self
                .fix
                .request("GET", &uri, Some(&self.fix.admin_token))
                .await;
            assert_eq!(status, axum::http::StatusCode::OK, "{body}");
            body["data"].clone()
        })
    }

    pub fn bind(&self, preview: &Value, id: u64) {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let (status, body) = self
                .fix
                .request_with_body(
                    "POST",
                    "/api/v1/reidentify",
                    Some(&self.fix.admin_token),
                    serde_json::json!({
                        "media_item_id": preview["media_item_id"],
                        "source_revision": preview["source_revision"],
                        "files": preview["groups"][0]["files"],
                        "title_ref": format!("tmdb:movie:{id}"),
                    }),
                )
                .await;
            assert_eq!(status, axum::http::StatusCode::OK, "{body}");
            assert_eq!(body["data"]["claimed"], 1);
        });
    }
}
