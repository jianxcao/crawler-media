use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;
use store::test_support::{FixtureFile, publish_fixture_file};

fn app(tmp: &tempfile::TempDir) -> axum::Router {
    router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ))
}

async fn put_inplace(app: &axum::Router, _tmp: &tempfile::TempDir, watch: &std::path::Path) {
    let saved = set_transfer_settings(&app, json!({"transfer_mode": "hardlink", "scrape": true, "watch_inplace": watch.display().to_string()})).await;
    assert_eq!(saved.status(), StatusCode::OK);
}

async fn tick(app: &axum::Router, now: i64) {
    for _ in 0..8 {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/jobs/tick?now={now}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn directory_round_trips_watch_inplace() {
    let tmp = tempfile::tempdir().unwrap();
    let watch = tmp.path().join("existing");
    let app = app(&tmp);
    put_inplace(&app, &tmp, &watch).await;
    let body = json_data(
        app.oneshot(request(
            "GET",
            "/api/v1/settings/transfer",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(body["watch_inplace"], watch.display().to_string());
}

#[tokio::test]
async fn scrape_job_does_not_write_placeholder_nfo_before_identification() {
    let tmp = tempfile::tempdir().unwrap();
    let watch = tmp.path().join("existing");
    std::fs::create_dir_all(&watch).unwrap();
    let video = watch.join("The.Matrix.1999.2160p.BluRay.mkv");
    std::fs::write(&video, b"stay").unwrap();
    let app = app(&tmp);
    put_inplace(&app, &tmp, &watch).await;
    // v1 只登记 Library 根内的文件：watch_inplace 目录必须同时是一个根。
    add_default_root(&app, "movie", &watch).await;
    tick(&app, 1).await;

    assert_eq!(std::fs::read(&video).unwrap(), b"stay");
    let nfo = watch.join("The.Matrix.1999.2160p.BluRay.nfo");
    assert!(
        !nfo.exists(),
        "unidentified file should not get a placeholder NFO"
    );

    let listed = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/ledger",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let rows = json_data(listed).await;
    let row = &rows.as_array().unwrap()[0];
    assert_eq!(row["media_title"], "The Matrix");
    assert_eq!(row["path"], video.display().to_string());

    let wall = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/libraries",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let libs = json_body(wall).await;
    let movie_id = libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "movie")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let wall = app
        .oneshot(request(
            "GET",
            &format!("/api/v1/libraries/{movie_id}/items"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let cards = json_body(wall).await;
    assert_eq!(cards["data"][0]["title"], "The Matrix");
}

#[tokio::test]
async fn scrape_job_scans_extra_library_root_into_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let extra = tmp.path().join("disk2/movies");
    std::fs::create_dir_all(&extra).unwrap();
    let video = extra.join("The.Matrix.1999.2160p.BluRay.mkv");
    std::fs::write(&video, b"stay").unwrap();
    let app = app(&tmp);
    let saved =
        set_transfer_settings(&app, json!({"transfer_mode": "hardlink", "scrape": true})).await;
    assert_eq!(saved.status(), StatusCode::OK);
    add_default_root(&app, "movie", &extra).await;
    tick(&app, 1).await;

    assert_eq!(std::fs::read(&video).unwrap(), b"stay");
    assert!(
        !tmp.path()
            .join("data/library/movies/The.Matrix.1999.2160p.BluRay.mkv")
            .exists()
    );

    let listed = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/ledger",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let rows = json_data(listed).await;
    let row = &rows.as_array().unwrap()[0];
    assert_eq!(row["media_title"], "The Matrix");
    assert_eq!(row["path"], video.display().to_string());

    let wall = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/libraries",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let libs = json_body(wall).await;
    let movie_id = libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "movie")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let wall = app
        .oneshot(request(
            "GET",
            &format!("/api/v1/libraries/{movie_id}/items"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let cards = json_body(wall).await;
    assert_eq!(cards["data"][0]["title"], "The Matrix");
}

#[tokio::test]
async fn scrape_job_scans_extra_tv_root_for_strm_into_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let extra = tmp.path().join("disk2/tv");
    let nested = extra.join("The Long Watch/Season 01");
    std::fs::create_dir_all(&nested).unwrap();
    let video = nested.join("The Long Watch - S01E01 - 第 1 集.strm");
    std::fs::write(&video, b"stay").unwrap();
    let app = app(&tmp);
    let saved =
        set_transfer_settings(&app, json!({"transfer_mode": "hardlink", "scrape": true})).await;
    assert_eq!(saved.status(), StatusCode::OK);
    add_default_root(&app, "tv", &extra).await;
    tick(&app, 1).await;

    assert_eq!(std::fs::read(&video).unwrap(), b"stay");
    assert!(
        !tmp.path()
            .join("data/library/tv/The.Long.Watch.S01E01.1080p.mkv")
            .exists()
    );
    assert!(
        !nested
            .join("The Long Watch - S01E01 - 第 1 集.nfo")
            .exists()
    );

    let listed = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/ledger",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let rows = json_data(listed).await;
    let row = &rows.as_array().unwrap()[0];
    assert_eq!(row["media_title"], "The Long Watch");
    assert_eq!(row["path"], video.display().to_string());
}

struct OrbitCatalog(media::CatalogHit);
impl api::catalog::Catalog for OrbitCatalog {
    fn search_movie(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn search_tv(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![self.0.clone()])
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(
        &self,
        _kind: domain::MediaKind,
        _id: &str,
    ) -> Result<Option<domain::Media>, String> {
        Ok(Some(self.0.media.clone()))
    }
    fn metadata(
        &self,
        _kind: domain::MediaKind,
        _id: &str,
    ) -> Result<Option<media::ItemMeta>, String> {
        Ok(Some(media::ItemMeta {
            overview: Some("Show summary".into()),
            ..Default::default()
        }))
    }
    fn season_details(&self, _id: &str, _season: u32) -> Result<Vec<media::EpisodeMeta>, String> {
        Ok(vec![
            media::EpisodeMeta {
                episode_number: 1,
                name: Some("Pilot Episode".into()),
                overview: Some("Episode summary".into()),
                ..Default::default()
            },
            media::EpisodeMeta {
                episode_number: 2,
                name: Some("Second Episode".into()),
                overview: Some("Episode 2 summary".into()),
                ..Default::default()
            },
        ])
    }
}

#[tokio::test]
async fn watch_scrape_job_rebinds_incoming_rows_to_canonical_media_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let old_canonical = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Tv,
        title: "Canonical Orbit".into(),
        year: Some(2022),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("orbit-tmdb".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let hit = media::CatalogHit {
        media: domain::Media {
            id: domain::MediaId::new(),
            kind: domain::MediaKind::Tv,
            title: "Orbit".into(),
            year: Some(2022),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("orbit-tmdb".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        },
        poster_path: None,
        backdrop_path: None,
        rating: None,
        overview: None,
    };
    let catalog = Arc::new(OrbitCatalog(hit));
    let state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    )
    .with_catalog(catalog);

    {
        let store_arc = state.store();
        let store = store_arc.lock();
        store.insert_media(&old_canonical).unwrap();
        // 预存 canonical Media 在已有其它目录的既有分集（Season 1 Episode 2），使 rows 非空
        publish_fixture_file(
            &store,
            &FixtureFile {
                file_id: Some(domain::LedgerId::new()),
                media_id: old_canonical.id,
                path: tmp
                    .path()
                    .join("data/library/tv/Orbit/Season 1/Orbit.S01E02.1080p.mkv")
                    .display()
                    .to_string(),
                season: Some(1),
                episode: Some(2),
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: domain::QualitySource::Probe,
                confidence: domain::Confidence::High,
                filter_score: None,
                ..Default::default()
            },
        )
        .unwrap();
    }
    let app = router(state.clone());

    let watch = tmp.path().join("extra-tv");
    let watch_season = watch.join("Orbit").join("Season 1");
    std::fs::create_dir_all(&watch_season).unwrap();
    let video = watch_season.join("Orbit.S01E01.1080p.mkv");
    std::fs::write(&video, b"orbit episode").unwrap();
    std::fs::write(
        watch.join("Orbit/tvshow.nfo"),
        b"<tvshow><title>Orbit</title></tvshow>",
    )
    .unwrap();
    let directory = set_transfer_settings(&app, json!({"transfer_mode": "hardlink", "scrape": true, "watch_inplace": tmp.path().join("empty-watch").display().to_string()})).await;
    assert_eq!(directory.status(), StatusCode::OK);
    add_default_root(&app, "tv", &watch).await;
    tick(&app, 1).await;

    let store_arc = state.store();
    let store = store_arc.lock();
    let video_path = video.display().to_string();
    let coverage = store
        .file_coverage_by_path(&video_path)
        .unwrap()
        .expect("watch job inserts the new episode");
    assert_eq!(
        coverage.file.media_id, old_canonical.id,
        "TV episode ledger should use the retained canonical Media"
    );
    let episode_nfo =
        std::fs::read_to_string(video.with_extension("nfo")).expect("new episode NFO is published");
    assert!(
        episode_nfo.contains("Pilot Episode"),
        "episode NFO must use fetched canonical episode metadata: {episode_nfo}"
    );
}
