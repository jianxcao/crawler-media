//! E3: unowned catalog stills must not become nonexistent local API paths.
#[path = "episode_still_proxy/support.rs"]
mod support;
use axum::{body::to_bytes, http::StatusCode};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use support::*;

#[test]
fn unowned_library_episode_stills_use_proxy_and_owned_still_stays_public() {
    let fixture = Fixture::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let episodes = get_json(
            fixture.app(),
            &format!(
                "/api/v1/libraries/{}/items/{}/episodes",
                fixture.owned.library_id, fixture.owned.media_id
            ),
        )
        .await;
        let rows = episodes["data"].as_array().unwrap();
        for (episode, _, expected) in REFERENCES {
            let row = rows
                .iter()
                .find(|row| row["episode_number"] == episode)
                .unwrap();
            assert_eq!(row["owned"], false);
            assert!(row["file_ids"].as_array().unwrap().is_empty());
            assert_proxy_reference(row["still_url"].as_str().unwrap(), expected);
        }
        let owned = rows.iter().find(|row| row["episode_number"] == 1).unwrap();
        assert_eq!(owned["owned"], true);
        let still = owned["still_url"].as_str().unwrap();
        assert_eq!(still, format!("/stills/{}", fixture.owned.file_id));
        let image = get(fixture.app(), &format!("/api/v1{still}"), false).await;
        assert_eq!(image.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(image.into_body(), 1024).await.unwrap().as_ref(),
            b"local-still-bytes"
        );
    });
}

#[test]
fn catalog_season_episode_stills_use_the_same_proxy_contract() {
    let fixture = Fixture::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let season = get_json(fixture.app(), "/api/v1/media/tv/123/season/1").await;
        let rows = season["data"]["episodes"].as_array().unwrap();
        assert_eq!(rows.len(), REFERENCES.len());
        for (episode, _, expected) in REFERENCES {
            let row = rows
                .iter()
                .find(|row| row["episode_number"] == episode)
                .unwrap();
            assert_proxy_reference(row["still_url"].as_str().unwrap(), expected);
        }
        assert_eq!(fixture.catalog.origin_fetches.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn season_detail_cache_miss_does_not_origin_fetch() {
    use std::sync::atomic::AtomicUsize;
    let catalog = Arc::new(support::ColdSeason {
        origin_fetches: AtomicUsize::new(0),
    });
    let (_dir, app) = support::catalog_app(catalog.clone());
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let season = get_json(app, "/api/v1/media/tv/123/season/1").await;
        assert_eq!(season["data"]["episodes"].as_array().unwrap().len(), 0);
        assert_eq!(catalog.origin_fetches.load(Ordering::SeqCst), 0);
    });
}
