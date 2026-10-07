//! Public Media detail links resolve TMDB aliases within the requested kind.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use api::catalog::Catalog;
use api::{Store, router};
use axum::http::StatusCode;
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use media::CatalogHit;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::catalog::hit;
use super::common::{Fixtures, json_data, request, state};

const TMDB_ID: &str = "603";

struct TemporaryIdCatalog;

impl Catalog for TemporaryIdCatalog {
    fn search_movie(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }

    fn search_tv(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }

    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }

    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }

    fn details(&self, kind: MediaKind, tmdb_id: &str) -> Result<Option<Media>, String> {
        if tmdb_id != TMDB_ID {
            return Ok(None);
        }
        // Real Catalog details can have a temporary UUID rather than the
        // canonical UUID referenced by the Library ledger. Generate a fresh
        // one on every call, so links cannot pass via the incoming-id fallback.
        Ok(Some(hit(kind.as_str(), "Catalog Media", tmdb_id).media))
    }
}

fn seed_owned_media(store: &Store, root: &Path, kind: MediaKind) -> Value {
    let media = hit(kind.as_str(), "Canonical Media", TMDB_ID).media;
    store.insert_media(&media).unwrap();
    let library_root = root.join(kind.as_str());
    std::fs::create_dir_all(&library_root).unwrap();
    let library_root_string = library_root.display().to_string();
    let library_name = format!("{} Library", kind.as_str());
    let library = store
        .create_library(
            kind,
            &library_name,
            &[library_root_string.as_str()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let video = library_root.join("Media.mkv");
    std::fs::write(&video, b"fixture").unwrap();
    let is_tv = kind == MediaKind::Tv;
    store
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id: media.id,
            path: video.display().to_string(),
            season: is_tv.then_some(1),
            episode: is_tv.then_some(1),
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
        })
        .unwrap();
    json!({
        "library_id": library.id,
        "library_name": library_name,
        "media_item_id": media.id.to_string(),
    })
}

async fn assert_detail_links(requested_kind: MediaKind, first_kind: MediaKind) {
    let tmp = tempfile::tempdir().unwrap();
    let state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    )
    .with_catalog(Arc::new(TemporaryIdCatalog));
    let second_kind = if first_kind == MediaKind::Movie {
        MediaKind::Tv
    } else {
        MediaKind::Movie
    };
    let (expected, other) = {
        let shared_store = state.store();
        let store = shared_store.lock();
        let first = seed_owned_media(&store, tmp.path(), first_kind);
        let second = seed_owned_media(&store, tmp.path(), second_kind);
        // The fixture must contain two distinct canonical identities and two
        // independent owners, even though their numeric TMDB aliases collide.
        assert_ne!(first["media_item_id"], second["media_item_id"]);
        assert_ne!(first["library_id"], second["library_id"]);
        if requested_kind == first_kind {
            (first, second)
        } else {
            (second, first)
        }
    };
    let temporary = TemporaryIdCatalog
        .details(requested_kind, TMDB_ID)
        .unwrap()
        .unwrap();
    assert_ne!(
        temporary.id.to_string(),
        expected["media_item_id"].as_str().unwrap()
    );
    assert_ne!(
        temporary.id.to_string(),
        other["media_item_id"].as_str().unwrap()
    );
    assert!(
        state
            .store()
            .lock()
            .get_media(temporary.id)
            .unwrap()
            .is_none()
    );

    let response = router(state)
        .oneshot(request(
            "GET",
            &format!("/api/v1/media/{}/{TMDB_ID}", requested_kind.as_str()),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data = json_data(response).await;
    assert_eq!(data["item"]["kind"], requested_kind.as_str());
    assert_eq!(data["item"]["tmdb_id"], TMDB_ID);
    // Exact equality detects both a missing correct link and any leaked link
    // to the other kind, including an otherwise plausible Library name/UUID.
    assert_eq!(
        data["library_links"],
        json!([expected]),
        "{} detail must only link its canonical Library ({} inserted first); other-kind link: {other}",
        requested_kind.as_str(),
        first_kind.as_str(),
    );
}

#[tokio::test]
async fn tv_detail_links_only_tv_library_for_colliding_tmdb_alias() {
    // Cover both insertion orders: an unscoped lookup cannot be made correct
    // merely by whichever kind SQLite happens to return first.
    for first_kind in [MediaKind::Movie, MediaKind::Tv] {
        assert_detail_links(MediaKind::Tv, first_kind).await;
    }
}

#[tokio::test]
async fn movie_detail_links_only_movie_library_for_colliding_tmdb_alias() {
    for first_kind in [MediaKind::Tv, MediaKind::Movie] {
        assert_detail_links(MediaKind::Movie, first_kind).await;
    }
}
