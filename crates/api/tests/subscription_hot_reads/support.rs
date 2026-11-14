use domain::{Coverage, FetchMode, Media, MediaId, MediaKind, Subscribe, SubscribeId, UserId};
use parking_lot::Mutex;
use std::{collections::HashMap, sync::Arc};
#[path = "../management/common.rs"]
mod common;

pub struct Fixture {
    pub _dir: tempfile::TempDir,
    pub state: api::ApiState,
    pub user: UserId,
    pub media: MediaId,
}
impl Fixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let state = common::state(
            dir.path(),
            Arc::new(common::Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            Arc::new(downloader::MemoryDownloader::new(dir.path().join("stage"))),
        )
        .with_catalog(Arc::new(api::catalog::EmptyCatalog));
        let store = state.store();
        let user = store.list_users().unwrap()[0].id;
        let media = MediaId::new();
        store
            .insert_media(&Media {
                id: media,
                kind: MediaKind::Tv,
                title: "Hot Subscribe".into(),
                year: None,
                original_title: None,
                aliases: vec![],
                tmdb_id: Some("12".into()),
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        Self {
            _dir: dir,
            state,
            user,
            media,
        }
    }
    pub fn add(&self, coverage: Coverage) -> SubscribeId {
        let store = self.state.store();
        let id = SubscribeId::new();
        store
            .insert_subscribe(&Subscribe {
                id,
                user_id: self.user,
                media_id: self.media,
                coverage,
                fetch_mode: FetchMode::Search,
                filter_id: store.list_filters().unwrap()[0].id,
                wash_cut: false,
                wash_cut_filter_id: None,
                keep_old_versions: false,
                full_season_pack: false,
                downloader_id: None,
                library_id: None,
                tracking_state: "active".into(),
                follow_future: false,
                search_interval_secs: 1800,
            })
            .unwrap();
        id
    }
    pub fn cache(&self, source: &str, key: &str, body: &str, at: i64) {
        self.state
            .store()
            .put_catalog_cache(source, key, body, at, Some(at + 1))
            .unwrap();
    }
    pub async fn list(
        &self,
    ) -> (
        serde_json::Value,
        media_server::query_metrics::QueryMetricsSnapshot,
    ) {
        use tower::ServiceExt;
        let timing = media_server::provider::QueryTiming::start();
        let metrics = media_server::query_metrics::QueryMetrics::new(timing, true);
        let mut request = common::request(
            "GET",
            "/api/v1/subscriptions",
            Some("management-secret"),
            serde_json::Value::Null,
        );
        request.extensions_mut().insert(timing);
        request.extensions_mut().insert(metrics.clone());
        let response = api::router(self.state.clone())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        (common::json_data(response).await, metrics.snapshot())
    }
}
pub fn open() -> Coverage {
    Coverage::Tv {
        season: 1,
        episode_from: 1,
        episode_to: None,
    }
}
pub fn body(count: i32) -> String {
    serde_json::json!({"id":12,"name":"Hot Subscribe","seasons":[{"season_number":1,"episode_count":count}]}).to_string()
}
