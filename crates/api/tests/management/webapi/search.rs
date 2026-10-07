use super::*;

#[tokio::test]
async fn discover_without_catalog_returns_empty_sections() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let response = send(
        &app,
        "GET",
        "/api/v1/search/titles?keyword=test",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["ok"], true);
    assert!(body["data"]["titles"].is_array());
}

#[tokio::test]
async fn empty_keyword_torrent_search_browses_enabled_sites() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let response = send(
        &app,
        "GET",
        "/api/v1/search/torrents",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["data"]["keyword"], "");
}

#[tokio::test]
async fn torrent_search_stream_emits_events() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let response = send(
        &app,
        "GET",
        "/api/v1/search/torrents/stream?keyword=test",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 64)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("event:start") || text.contains("start"));
}

/// 单源 fake catalog（U3 provider 定向测试）。
struct SingleSourceCatalog {
    name: &'static str,
    movie: Option<domain::Media>,
    show: Option<domain::Media>,
}

impl api::catalog::Catalog for SingleSourceCatalog {
    fn source_name(&self) -> &'static str {
        self.name
    }
    fn search_movie(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(self
            .movie
            .clone()
            .map(|media| media::CatalogHit {
                media,
                poster_path: None,
                backdrop_path: None,
                rating: None,
                overview: None,
            })
            .into_iter()
            .collect())
    }
    fn search_tv(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(self
            .show
            .clone()
            .map(|media| media::CatalogHit {
                media,
                poster_path: None,
                backdrop_path: None,
                rating: None,
                overview: None,
            })
            .into_iter()
            .collect())
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
        _tmdb_id: &str,
    ) -> Result<Option<domain::Media>, String> {
        Ok(self.movie.clone())
    }
}

fn media_of(name: &'static str, kind: domain::MediaKind, tmdb: &str) -> domain::Media {
    domain::Media {
        id: domain::MediaId::new(),
        kind,
        title: name.into(),
        year: Some(2020),
        original_title: None,
        tmdb_id: Some(tmdb.into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn fanout_app(tmp: &tempfile::TempDir) -> axum::Router {
    let tmdb = Arc::new(SingleSourceCatalog {
        name: "tmdb",
        movie: Some(media_of("Inception", domain::MediaKind::Movie, "27205")),
        show: None,
    });
    let douban = Arc::new(SingleSourceCatalog {
        name: "douban",
        movie: Some(media_of("盗梦空间", domain::MediaKind::Movie, "27205")),
        show: None,
    });
    let fanout = api::catalog::FanoutCatalog::new(vec![tmdb, douban]);
    router(
        state(
            tmp.path(),
            Arc::new(Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )
        .with_catalog(fanout),
    )
}

#[tokio::test]
async fn title_search_provider_filters_to_single_source() {
    let tmp = tempfile::tempdir().unwrap();
    let app = fanout_app(&tmp);
    let response = send(
        &app,
        "GET",
        "/api/v1/search/titles?keyword=inception&provider=tmdb",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let providers = body["data"]["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 1, "定向搜索只列一个 provider");
    assert_eq!(providers[0]["provider"], "tmdb");
    assert_eq!(providers[0]["ok"], true);
    assert_eq!(providers[0]["count"], 1);
    let titles = body["data"]["titles"].as_array().unwrap();
    assert_eq!(titles.len(), 1);
    assert_eq!(titles[0]["provider"], "tmdb");
    assert_eq!(titles[0]["title"], "Inception");
}

#[tokio::test]
async fn title_search_unknown_provider_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let app = fanout_app(&tmp);
    let response = send(
        &app,
        "GET",
        "/api/v1/search/titles?keyword=inception&provider=nope",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert_eq!(body["error"]["code"], "search.provider");

    let history = json_body(
        send(
            &app,
            "GET",
            "/api/v1/search/history",
            Some("management-secret"),
            Value::Null,
        )
        .await,
    )
    .await;
    assert!(
        history["data"]["items"].as_array().unwrap().is_empty(),
        "a rejected provider must not create a history entry"
    );
}

#[tokio::test]
async fn title_search_default_fanout_reports_each_source() {
    let tmp = tempfile::tempdir().unwrap();
    let app = fanout_app(&tmp);
    let response = send(
        &app,
        "GET",
        "/api/v1/search/titles?keyword=inception",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    let body = json_body(response).await;
    let providers = body["data"]["providers"].as_array().unwrap();
    let names: Vec<&str> = providers
        .iter()
        .filter_map(|p| p["provider"].as_str())
        .collect();
    assert!(
        names.contains(&"tmdb") && names.contains(&"douban"),
        "fanout 应逐源报告: {names:?}"
    );
}

#[tokio::test]
async fn search_history_snapshot_round_trip_and_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    assert_transient_search_not_saved(&app).await;
    let id = save_empty_search(&app).await;
    assert_empty_snapshot(&app, &id).await;
    assert_history_deletion(&app, &id).await;
}

#[tokio::test]
async fn title_search_snapshot_captures_results() {
    let tmp = tempfile::tempdir().unwrap();
    let app = fanout_app(&tmp);
    let searched = send(
        &app,
        "GET",
        "/api/v1/search/titles?keyword=inception",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(searched.status(), StatusCode::OK);

    let history = json_body(
        send(
            &app,
            "GET",
            "/api/v1/search/history",
            Some("management-secret"),
            Value::Null,
        )
        .await,
    )
    .await;
    let items = history["data"]["items"].as_array().unwrap();
    let id = items[0]["id"].as_str().unwrap().to_string();
    let snapshot = json_body(
        send(
            &app,
            "GET",
            &format!("/api/v1/search/history/{id}?vertical=titles"),
            Some("management-secret"),
            Value::Null,
        )
        .await,
    )
    .await;
    assert_eq!(snapshot["data"]["vertical"], "titles");
    let titles = snapshot["data"]["items"].as_array().unwrap();
    assert_eq!(titles.len(), 2, "fanout 双源各 1 条");
    let names: Vec<&str> = titles.iter().filter_map(|t| t["title"].as_str()).collect();
    assert!(names.contains(&"Inception") && names.contains(&"盗梦空间"));
}

async fn assert_transient_search_not_saved(app: &axum::Router) {
    // 未明确请求保存时，空搜索结果不进入历史。
    let searched = send(
        &app,
        "GET",
        "/api/v1/search/torrents?keyword=transient",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(searched.status(), StatusCode::OK);

    let empty_history = json_body(
        send(
            &app,
            "GET",
            "/api/v1/search/history",
            Some("management-secret"),
            Value::Null,
        )
        .await,
    )
    .await;
    assert!(
        empty_history["data"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

async fn save_empty_search(app: &axum::Router) -> String {
    // 无启用站点 → 空结果，但显式开启历史后仍记录历史与快照。
    let searched = send(
        &app,
        "GET",
        "/api/v1/search/torrents?keyword=matrix&save_history=true",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(searched.status(), StatusCode::OK);

    let history = json_body(
        send(
            &app,
            "GET",
            "/api/v1/search/history",
            Some("management-secret"),
            Value::Null,
        )
        .await,
    )
    .await;
    let items = history["data"]["items"].as_array().unwrap();
    assert!(!items.is_empty(), "应有历史记录");
    let id = items[0]["id"].as_str().unwrap().to_string();
    assert_eq!(items[0]["query"], "matrix");

    id
}

async fn assert_empty_snapshot(app: &axum::Router, id: &str) {
    // 快照回放。
    let snapshot = json_body(
        send(
            &app,
            "GET",
            &format!("/api/v1/search/history/{id}?vertical=torrents"),
            Some("management-secret"),
            Value::Null,
        )
        .await,
    )
    .await;
    assert_eq!(snapshot["data"]["vertical"], "torrents");
    assert_eq!(snapshot["data"]["keyword"], "matrix");
    assert_eq!(snapshot["data"]["total"], 0);
}

async fn assert_history_deletion(app: &axum::Router, id: &str) {
    // 删除单条：快照 404 + 历史消失。
    let deleted = send(
        &app,
        "DELETE",
        &format!("/api/v1/search/history/{id}"),
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(deleted.status(), StatusCode::OK);
    let gone = send(
        &app,
        "GET",
        &format!("/api/v1/search/history/{id}?vertical=torrents"),
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);

    // 清空。
    let cleared = send(
        &app,
        "DELETE",
        "/api/v1/search/history",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(cleared.status(), StatusCode::OK);
    let history = json_body(
        send(
            &app,
            "GET",
            "/api/v1/search/history",
            Some("management-secret"),
            Value::Null,
        )
        .await,
    )
    .await;
    assert_eq!(
        history["data"]["items"].as_array().unwrap().len(),
        0,
        "清空后历史应为空"
    );
}
