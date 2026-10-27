//! Jellyfin catalog 测试模块根：只放共享 helper 与子模块声明。
//!
//! 按职责拆分自原 1833 行的 `catalog.rs`（task-17 Step 3 硬上限）；
//! 子模块用 `#[path]` 显式指向 `catalog/`，与 `jellyfin_paged_metadata.rs` 同风格。

use super::*;

async fn get_json(app: &axum::Router, uri: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    json(response).await
}

fn new_media(kind: MediaKind, title: &str) -> Media {
    Media {
        id: MediaId::new(),
        kind,
        title: title.into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn insert_episode(
    store: &Store,
    root: &Path,
    media_id: MediaId,
    season: u32,
    episode: u32,
) -> String {
    let path = root.join(format!("Show S{season:02}E{episode:02}.mkv"));
    std::fs::write(&path, b"episode").unwrap();
    let spec = store::test_support::FixtureFile {
        media_id,
        path: path.display().to_string(),
        season: Some(season),
        episode: Some(episode),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
        transfer_mode: None,
        source_path: None,
        file_id: None,
    };
    store::test_support::publish_fixture_file(store, &spec).unwrap();
    unit_id(media_id, season as i32, episode as i32)
}

#[path = "catalog/artwork_metadata.rs"]
mod artwork_metadata;
#[path = "catalog/expansion.rs"]
mod expansion;
#[path = "catalog/favorite_marks.rs"]
mod favorite_marks;
#[path = "catalog/marks_rollup.rs"]
mod marks_rollup;
#[path = "catalog/resume.rs"]
mod resume;
#[path = "catalog/tv_season_filter.rs"]
mod tv_season_filter;

#[path = "marks.rs"]
mod marks;
