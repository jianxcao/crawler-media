use std::io::{Cursor, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use api::catalog::{Catalog, DoubanCatalog, FanoutCatalog, TmdbCatalog};
use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use downloader::MemoryDownloader;
use image::{ImageFormat, Rgba, RgbaImage};
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media::{CatalogGet, Douban, Tmdb, TmdbError};
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "polish-admin-token";

struct OfflineFetcher;

impl Fetcher for OfflineFetcher {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline test".into()))
    }
}

fn base_state(root: &Path) -> ApiState {
    let store = Store::open(root.join("data")).unwrap();
    ApiState::new(
        store,
        TOKEN.into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(OfflineFetcher),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
    )
    .unwrap()
}

async fn send_json(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    let req_body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("content-type", "application/json")
                .body(req_body)
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn build_logo_fixture_png() -> Vec<u8> {
    // 60x100 opaque white canvas -> active content only in x = 10..50, y = 35..65 (70% vertical padding)
    let mut img = RgbaImage::from_pixel(60, 100, Rgba([255, 255, 255, 255]));
    for y in 35..50 {
        for x in 10..22 {
            img.put_pixel(x, y, Rgba([20, 140, 255, 255])); // saturated blue
        }
        for x in 22..32 {
            img.put_pixel(x, y, Rgba([255, 110, 20, 255])); // saturated orange
        }
    }
    for y in 50..65 {
        for x in 10..50 {
            img.put_pixel(x, y, Rgba([22, 22, 25, 255])); // dark charcoal wordmark
        }
    }
    let mut out = Vec::new();
    img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
        .unwrap();
    out
}

#[tokio::test]
async fn brand_logo_proxy_strips_white_border_crops_padding_preserves_color_and_lifts_dark_text() {
    let tmp = tempfile::tempdir().unwrap();
    let png_bytes = build_logo_fixture_png();
    let state = base_state(tmp.path()).with_image_fetch(Arc::new(move |url: &str| {
        assert_eq!(url, "https://image.tmdb.org/t/p/w300/test.png");
        Ok(("image/png".to_string(), png_bytes.clone()))
    }));
    let app = router(state);

    let normalized = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/images/proxy?url=https://image.tmdb.org/t/p/w300/test.png&variant=brand-logo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(normalized.status(), StatusCode::OK);
    assert_eq!(normalized.headers()["content-type"], "image/png");
    let norm_bytes = to_bytes(normalized.into_body(), usize::MAX).await.unwrap();
    let decoded = image::load_from_memory(&norm_bytes).unwrap().to_rgba8();

    assert_eq!(decoded.dimensions(), (40, 30));
    assert_eq!(decoded.get_pixel(39, 0).0[3], 0);
    assert_eq!(decoded.get_pixel(2, 2).0, [20, 140, 255, 255]);
    assert_eq!(decoded.get_pixel(15, 2).0, [255, 110, 20, 255]);
    let wordmark = decoded.get_pixel(20, 22).0;
    assert!(wordmark[0] >= 225 && wordmark[1] >= 225 && wordmark[2] >= 225);
    assert_eq!(wordmark[3], 255);

    let raw = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/images/proxy?url=https://image.tmdb.org/t/p/w300/test.png")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(raw.status(), StatusCode::OK);
    let raw_bytes = to_bytes(raw.into_body(), usize::MAX).await.unwrap();
    let raw_decoded = image::load_from_memory(&raw_bytes).unwrap().to_rgba8();
    assert_eq!(raw_decoded.dimensions(), (60, 100));
}

#[derive(Clone, Copy)]
enum TmdbMode {
    Matched,
    Missing,
    Ambiguous,
}

struct FakeMetadataHttp {
    tmdb_mode: TmdbMode,
}

impl CatalogGet for FakeMetadataHttp {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
        if path == "/rexxar/api/v2/movie/35235320?for_mobile=1" {
            return Ok(json!({
                "id": "35235320",
                "title": "两京十五日",
                "original_title": "两京十五日",
                "year": "2026",
                "type": "movie",
                "subtype": "tv",
                "episodes_count": 26,
                "intro": "大明洪熙元年惊险千里奔袭。",
                "aka": ["15 Days in Two Capitals"]
            })
            .to_string());
        }
        if path == "/rexxar/api/v2/movie/35235320/celebrities?for_mobile=1" {
            return Ok(json!({ "actors": [], "directors": [] }).to_string());
        }
        if path.starts_with("/search/tv?") {
            return Ok(search_tv_fixture(self.tmdb_mode));
        }
        if path.starts_with("/search/movie?") || path.starts_with("/tv/popular") {
            return Ok(json!({
                "page": 1, "total_pages": 1, "total_results": 0, "results": []
            })
            .to_string());
        }
        if path.starts_with("/tv/203483/images") {
            return Ok(json!({ "id": 203483, "posters": [], "backdrops": [] }).to_string());
        }
        if path.starts_with("/tv/203483/season/1") {
            return Ok(json!({
                "id": 91001,
                "name": "第 1 季",
                "season_number": 1,
                "episodes": [
                    {
                        "episode_number": 1,
                        "name": "第一集：南京起程",
                        "overview": "洪熙元年，太子朱瞻基遇袭。",
                        "still_path": "/ep1.jpg",
                        "air_date": "2026-05-01"
                    },
                    {
                        "episode_number": 2,
                        "name": "第二集：运河夜渡",
                        "overview": "吴定缘与于谦会合。",
                        "still_path": "/ep2.jpg",
                        "air_date": "2099-12-30"
                    }
                ]
            })
            .to_string());
        }
        if path.starts_with("/tv/66732/season/2") && path.contains("language=en-US") {
            return Ok(json!({
                "id": 92002,
                "name": "Season 2",
                "season_number": 2,
                "episodes": [
                    {
                        "episode_number": 1,
                        "name": "Chapter One: MADMAX",
                        "overview": "Will sees visions of the Upside Down.",
                        "still_path": "/s2e1.jpg",
                        "air_date": "2017-10-27"
                    },
                    {
                        "episode_number": 2,
                        "name": "Chapter Two: Trick or Treat, Freak",
                        "overview": "Eleven recalls her escape from the woods.",
                        "still_path": "/s2e2.jpg",
                        "air_date": "2017-10-27"
                    }
                ]
            })
            .to_string());
        }
        if path.starts_with("/tv/66732/season/2") {
            return Ok(json!({
                "id": 92002,
                "name": "第 2 季",
                "season_number": 2,
                "episodes": [
                    {
                        "episode_number": 1,
                        "name": "第一章：疯狂麦克斯",
                        "overview": "威尔再次看到颠倒世界的幻象。",
                        "still_path": "/s2e1.jpg",
                        "air_date": "2017-10-27"
                    },
                    {
                        "episode_number": 2,
                        "name": "第 2 集",
                        "overview": "",
                        "still_path": "/s2e2.jpg",
                        "air_date": "2017-10-27"
                    }
                ]
            })
            .to_string());
        }
        if path.starts_with("/tv/203483") {
            return Ok(json!({
                "id": 203483,
                "name": "两京十五日",
                "original_name": "两京十五日",
                "first_air_date": "2026-05-01",
                "overview": "TMDB 剧集简介",
                "vote_average": 8.3,
                "vote_count": 42,
                "seasons": [{
                    "season_number": 1,
                    "name": "第 1 季",
                    "episode_count": 26,
                    "air_date": "2026-05-01"
                }]
            })
            .to_string());
        }
        Err(TmdbError::NotFound(path.to_string()))
    }
}

fn search_tv_fixture(mode: TmdbMode) -> String {
    match mode {
        TmdbMode::Missing => {
            json!({ "page": 1, "total_pages": 1, "total_results": 0, "results": [] }).to_string()
        }
        TmdbMode::Matched => json!({
            "page": 1,
            "total_pages": 1,
            "total_results": 1,
            "results": [{
                "id": 203483,
                "name": "两京十五日",
                "original_name": "两京十五日",
                "first_air_date": "2026-05-01",
                "poster_path": "/liangjing.jpg",
                "overview": "TMDB 剧集简介"
            }]
        })
        .to_string(),
        TmdbMode::Ambiguous => json!({
            "page": 1,
            "total_pages": 1,
            "total_results": 2,
            "results": [
                {
                    "id": 203483,
                    "name": "两京十五日",
                    "original_name": "两京十五日",
                    "first_air_date": "2026-05-01",
                    "poster_path": "/a.jpg",
                    "overview": "版本 A"
                },
                {
                    "id": 209999,
                    "name": "两京十五日",
                    "original_name": "两京十五日",
                    "first_air_date": "2026-08-01",
                    "poster_path": "/b.jpg",
                    "overview": "版本 B"
                }
            ]
        })
        .to_string(),
    }
}

fn app_with_mode(root: &Path, mode: TmdbMode) -> axum::Router {
    let catalog_db = root.join("catalog.db");
    let tmdb =
        TmdbCatalog::new(Tmdb::new(FakeMetadataHttp { tmdb_mode: mode }, &catalog_db).unwrap());
    let douban =
        DoubanCatalog::new(Douban::new(FakeMetadataHttp { tmdb_mode: mode }, &catalog_db).unwrap());
    let sources: Vec<Arc<dyn Catalog>> = vec![tmdb, douban];
    router(base_state(root).with_catalog(FanoutCatalog::new(sources)))
}

#[tokio::test]
async fn douban_preview_and_detail_anchor_to_tmdb_with_seasons_and_persist_source_alias() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app_with_mode(tmp.path(), TmdbMode::Matched);

    let (status, preview) = send_json(
        &app,
        "POST",
        "/api/v1/subscriptions/title-preview",
        json!({ "title_ref": "douban:35235320" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["data"]["status"], "ready");
    assert_eq!(preview["data"]["media"]["kind"], "tv");
    assert_eq!(preview["data"]["media"]["tmdb_id"], "203483");
    assert_eq!(preview["data"]["media"]["douban_id"], "35235320");
    assert_eq!(preview["data"]["seasons"][0]["season_number"], 1);
    assert_eq!(preview["data"]["seasons"][0]["episode_count"], 26);

    let (d_status, detail) =
        send_json(&app, "GET", "/api/v1/media/douban/35235320", Value::Null).await;
    assert_eq!(d_status, StatusCode::OK, "{detail}");
    assert_eq!(detail["data"]["item"]["tmdb_id"], "203483");
    assert_eq!(detail["data"]["item"]["douban_id"], "35235320");
    assert_eq!(detail["data"]["seasons"][0]["season_number"], 1);

    let (c_status, created) = send_json(
        &app,
        "POST",
        "/api/v1/subscriptions",
        json!({
            "title_ref": "tmdb:tv:203483",
            "source_title_ref": "douban:35235320",
            "coverage": { "kind": "tv", "season": 1, "episode_from": 1, "episode_to": null }
        }),
    )
    .await;
    assert_eq!(c_status, StatusCode::CREATED, "{created}");
    assert_eq!(created["data"]["media"]["tmdb_id"], "203483");
    assert_eq!(created["data"]["media"]["douban_id"], "35235320");
}

#[tokio::test]
async fn douban_preview_synthesizes_season_one_without_tmdb_and_reports_ambiguous_ties() {
    let tmp_missing = tempfile::tempdir().unwrap();
    let app_missing = app_with_mode(tmp_missing.path(), TmdbMode::Missing);
    let (status, preview) = send_json(
        &app_missing,
        "POST",
        "/api/v1/subscriptions/title-preview",
        json!({ "title_ref": "douban:35235320" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["data"]["status"], "ready");
    assert_eq!(preview["data"]["media"]["kind"], "tv");
    assert_eq!(preview["data"]["media"]["tmdb_id"], Value::Null);
    assert_eq!(preview["data"]["media"]["douban_id"], "35235320");
    assert_eq!(preview["data"]["seasons"][0]["season_number"], 1);
    assert_eq!(preview["data"]["seasons"][0]["name"], "第 1 季");
    assert_eq!(preview["data"]["seasons"][0]["episode_count"], 26);

    let tmp_amb = tempfile::tempdir().unwrap();
    let app_amb = app_with_mode(tmp_amb.path(), TmdbMode::Ambiguous);
    let (a_status, amb) = send_json(
        &app_amb,
        "POST",
        "/api/v1/subscriptions/title-preview",
        json!({ "title_ref": "douban:35235320" }),
    )
    .await;
    assert_eq!(a_status, StatusCode::OK, "{amb}");
    assert_eq!(amb["data"]["status"], "ambiguous");
    assert_eq!(amb["data"]["candidates"].as_array().map(Vec::len), Some(2));
}

#[tokio::test]
async fn automation_readiness_exposes_zero_library_error_count_when_sites_unconfigured() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(base_state(tmp.path()));
    let (status, body) = send_json(
        &app,
        "GET",
        "/api/v1/subscriptions/automation-readiness",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["sites_configured"], false);
    assert_eq!(body["data"]["library_error_count"], 0);
}

fn spawn_slow_loopback_proxy() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        while let Ok((mut stream, _)) = listener.accept() {
            std::thread::spawn(move || {
                let mut buf = [0_u8; 512];
                let _ = stream.read(&mut buf);
                std::thread::sleep(Duration::from_millis(35));
                let _ = stream.write_all(
                    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            });
        }
    });
    addr
}

#[tokio::test]
async fn concurrent_proxy_diagnose_never_flashes_busy_and_preserves_deterministic_order() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(base_state(tmp.path()));
    let proxy_addr = spawn_slow_loopback_proxy();

    let (put_status, _) = send_json(
        &app,
        "PUT",
        "/api/v1/settings/proxy",
        json!({
            "proxy_url": format!("http://{proxy_addr}"),
            "douban_bypass": false,
        }),
    )
    .await;
    assert_eq!(put_status, StatusCode::OK);

    let (r1, r2, r3) = tokio::join!(
        send_json(&app, "POST", "/api/v1/settings/proxy/diagnose", json!({})),
        send_json(&app, "POST", "/api/v1/settings/proxy/diagnose", json!({})),
        send_json(&app, "POST", "/api/v1/settings/proxy/diagnose", json!({})),
    );
    let _ = send_json(
        &app,
        "PUT",
        "/api/v1/settings/proxy",
        json!({ "proxy_url": "", "douban_bypass": true }),
    )
    .await;

    let expected_names = vec![
        "TMDB (条目数据)",
        "TMDB (海报封面 CDN)",
        "Bangumi (动漫番组)",
        "AniList (海外番剧)",
        "豆瓣电影 (华语条目)",
    ];
    for (status, body) in [r1, r2, r3] {
        assert_eq!(status, StatusCode::OK, "{body}");
        let items = body["data"]["items"].as_array().expect("items array");
        let names: Vec<&str> = items.iter().filter_map(|it| it["name"].as_str()).collect();
        assert_eq!(names, expected_names);
        for item in items {
            let err_text = item["error"].as_str().unwrap_or_default();
            assert!(
                !err_text.contains("capacity exhausted"),
                "diagnose flashed false busy error: {item}"
            );
        }
    }
}

#[tokio::test]
async fn multi_season_detail_populates_overviews_with_en_fallback_and_ongoing_aired_count() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app_with_mode(tmp.path(), TmdbMode::Matched);

    // Cold-cache season 2 of 66732: ep1 has zh-CN overview, ep2 has empty zh-CN overview -> backfills from en-US!
    let (s2_status, s2) =
        send_json(&app, "GET", "/api/v1/media/tv/66732/season/2", Value::Null).await;
    assert_eq!(s2_status, StatusCode::OK, "{s2}");
    let eps = s2["data"]["episodes"].as_array().expect("episodes array");
    assert_eq!(eps.len(), 2);
    assert_eq!(eps[0]["name"], "第一章：疯狂麦克斯");
    assert_eq!(eps[0]["overview"], "威尔再次看到颠倒世界的幻象。");
    assert_eq!(eps[1]["name"], "Chapter Two: Trick or Treat, Freak");
    assert_eq!(
        eps[1]["overview"],
        "Eleven recalls her escape from the woods."
    );

    // Ongoing show 203483: 1 aired episode (2026-05-01 <= today) + 1 future episode (2099-12-30) -> aired_count == 1!
    let (p_status, preview) = send_json(
        &app,
        "POST",
        "/api/v1/subscriptions/title-preview",
        json!({ "title_ref": "tmdb:tv:203483" }),
    )
    .await;
    assert_eq!(p_status, StatusCode::OK, "{preview}");
    assert_eq!(preview["data"]["seasons"][0]["episode_count"], 26);
    assert_eq!(preview["data"]["seasons"][0]["aired_count"], 1);
}

#[tokio::test]
async fn brand_logo_proxy_strips_silver_gray_box_background() {
    let tmp = tempfile::tempdir().unwrap();
    let mut img = RgbaImage::from_pixel(50, 40, Rgba([196, 198, 202, 255]));
    for y in 12..28 {
        for x in 10..40 {
            img.put_pixel(x, y, Rgba([220, 40, 50, 255]));
        }
    }
    let mut png_bytes = Vec::new();
    img.write_to(&mut Cursor::new(&mut png_bytes), ImageFormat::Png)
        .unwrap();
    let state = base_state(tmp.path()).with_image_fetch(Arc::new(move |_url: &str| {
        Ok(("image/png".to_string(), png_bytes.clone()))
    }));
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/images/proxy?url=https://image.tmdb.org/t/p/w300/silver.png&variant=brand-logo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let norm_bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let decoded = image::load_from_memory(&norm_bytes).unwrap().to_rgba8();
    assert_eq!(decoded.dimensions(), (30, 16));
    assert_eq!(decoded.get_pixel(0, 0).0, [220, 40, 50, 255]);
}
