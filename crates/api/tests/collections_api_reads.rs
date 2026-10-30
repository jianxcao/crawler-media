//! Actual HTTP collection read work is measured inside store_wait workers.
use api::{ApiState, Store};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use domain::{Media, MediaId, MediaKind, User, UserId, UserRole};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media_server::{
    provider::QueryTiming,
    query_metrics::{QueryMetrics, QueryMetricsSnapshot},
};
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;
const TOKEN: &str = "collection-work-fake-token";
struct Offline;
impl Fetcher for Offline {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}
struct Fixture {
    dir: tempfile::TempDir,
    state: ApiState,
    collection: String,
    library: domain::LibraryId,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let user = User {
            id: UserId::new(),
            login: "collection-fixture".into(),
            enabled: true,
            role: UserRole::Admin,
        };
        store
            .insert_user_with_password(&user, "fake-password")
            .unwrap();
        store.set_user_token(user.id, TOKEN).unwrap();
        let root = dir.path().join("movies");
        std::fs::create_dir_all(&root).unwrap();
        let library = store
            .create_library(
                MediaKind::Movie,
                "Movies",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
            .id
            .parse()
            .unwrap();
        let collection = store.create_collection(user.id, "Fixture").unwrap();
        let state = ApiState::new(
            store,
            "collection-admin-fixture".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(Offline),
            Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
            dir.path().join("library"),
        )
        .unwrap();
        Self {
            dir,
            state,
            collection,
            library,
        }
    }
    fn add(&self, n: u64) -> MediaId {
        let store = self.state.store();
        let id: MediaId = format!("00000000-0000-0000-0000-{n:012}").parse().unwrap();
        store
            .insert_media(&Media {
                id,
                kind: MediaKind::Movie,
                title: format!("Movie {n}"),
                year: None,
                original_title: None,
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
                aliases: vec![],
            })
            .unwrap();
        let path = self.dir.path().join("movies").join(format!("{n}.mkv"));
        std::fs::write(&path, b"movie").unwrap();
        store::test_support::publish_fixture_file(
            &store,
            &store::test_support::FixtureFile {
                media_id: id,
                path: path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        store
            .add_collection_item(&self.collection, &id.to_string())
            .unwrap();
        id
    }
    async fn get(&self, uri: &str) -> (Value, QueryMetricsSnapshot) {
        let timing = QueryTiming::start();
        let metrics = QueryMetrics::new(timing, true);
        let mut req = Request::get(uri)
            .header("Authorization", format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(timing);
        req.extensions_mut().insert(metrics.clone());
        let res = api::router(self.state.clone()).oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = to_bytes(res.into_body(), 4 * 1024 * 1024).await.unwrap();
        let stats = metrics.snapshot();
        assert!(
            stats.statements.is_some_and(|n| n > 0),
            "worker measurement cannot be absent/zero: {stats:?}"
        );
        assert!(stats.vm_steps.is_some_and(|n| n > 0), "{stats:?}");
        (serde_json::from_slice(&bytes).unwrap(), stats)
    }
}
#[tokio::test]
async fn actual_collection_summary_statement_count_is_constant_for_five_and_fifty_members() {
    let f = Fixture::new();
    for n in 1..=5 {
        f.add(n);
    }
    let (small_body, small) = f.get("/api/v1/collections").await;
    assert_eq!(small_body["data"][0]["item_count"], 5);
    for n in 6..=50 {
        f.add(n);
    }
    let (large_body, large) = f.get("/api/v1/collections").await;
    assert_eq!(large_body["data"][0]["item_count"], 50);
    assert_eq!(
        large.statements, small.statements,
        "actualroute SQL N+1: {small:?} -> {large:?}"
    );
}
#[tokio::test]
async fn actual_collection_keyset_pages_keep_total_order_and_constant_statement_count() {
    let f = Fixture::new();
    for n in 1..=5 {
        f.add(n);
    }
    let uri = format!("/api/v1/collections/{}/items?limit=5", f.collection);
    let (small_body, small) = f.get(&uri).await;
    assert_eq!(small_body["data"]["items"].as_array().unwrap().len(), 5);
    assert_eq!(small_body["data"]["total"], 5);
    for n in 6..=50 {
        f.add(n);
    }
    let (large_body, large) = f.get(&uri).await;
    assert_eq!(large_body["data"]["items"].as_array().unwrap().len(), 5);
    assert_eq!(large_body["data"]["total"], 50);
    assert_eq!(large.statements, small.statements, "{small:?} -> {large:?}");
    let mut body = large_body;
    let mut ids = vec![];
    loop {
        assert_eq!(body["data"]["total"], 50);
        for row in body["data"]["items"].as_array().unwrap() {
            assert_eq!(row["library_id"], f.library.to_string());
            ids.push(row["media_item_id"].as_str().unwrap().to_owned());
        }
        let Some(cursor) = body["data"]["next_cursor"].as_str() else {
            break;
        };
        let query: url::Url = url::Url::parse_with_params(
            "http://fixture.invalid",
            [("limit", "5"), ("cursor", cursor)],
        )
        .unwrap();
        body = f
            .get(&format!(
                "/api/v1/collections/{}/items?{}",
                f.collection,
                query.query().unwrap()
            ))
            .await
            .0;
    }
    let expected: Vec<_> = (1..=50)
        .map(|n| format!("00000000-0000-0000-0000-{n:012}"))
        .collect();
    assert_eq!(
        ids, expected,
        "complete keyset traversal once each, no skipped/duplicate rows"
    );
}

#[tokio::test]
async fn collection_summary_uses_later_visible_member_cover_and_gallery_keeps_array() {
    let f = Fixture::new();
    let first = f.add(1);
    let second = f.add(2);
    let store = f.state.store();
    let art = f.dir.path().join("movies").join("z-artwork");
    std::fs::create_dir_all(&art).unwrap();
    let path = art.join("second.mkv");
    std::fs::write(&path, b"finite movie").unwrap();
    std::fs::write(art.join("poster.jpg"), b"finite poster").unwrap();
    let source = store::test_support::publish_fixture_file(
        &store,
        &store::test_support::FixtureFile {
            media_id: second,
            path: path.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    store
        .upsert_unit(
            store.user_id_by_token(TOKEN).unwrap().unwrap(),
            second,
            -1,
            -1,
            0,
            None,
            Some(true),
            None,
            None,
            None,
            false,
            10,
        )
        .unwrap();
    let (summary, _) = f.get("/api/v1/collections").await;
    assert_eq!(summary["data"][0]["item_count"], 2);
    assert_eq!(
        summary["data"][0]["cover_url"],
        format!("/posters/{}", source.file_id)
    );
    let (gallery, _) = f
        .get(&format!("/api/v1/collections/{}/gallery", f.collection))
        .await;
    let groups = gallery["data"].as_array().unwrap();
    assert_eq!(groups.len(), 2, "gallery remains a complete array contract");
    assert_eq!(groups[0]["media_item_id"], first.to_string());
    assert!(groups[0]["images"].as_array().unwrap().is_empty());
    assert_eq!(groups[1]["media_item_id"], second.to_string());
    assert_eq!(groups[1]["is_favorite"], true);
    assert_eq!(groups[1]["images"][0]["kind"], "poster");
}

#[tokio::test]
async fn collection_keyset_filters_hidden_members_before_count_and_page() {
    let f = Fixture::new();
    let visible = f.add(1);
    let store = f.state.store();
    let hidden_root = f.dir.path().join("hidden");
    std::fs::create_dir_all(&hidden_root).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "Hidden",
            &[hidden_root.to_str().unwrap()],
            "everyone",
            false,
            &[],
        )
        .unwrap();
    let hidden = MediaId::new();
    store
        .insert_media(&Media {
            id: hidden,
            kind: MediaKind::Movie,
            title: "Hidden member".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let path = hidden_root.join("hidden.mkv");
    std::fs::write(&path, b"finite hidden").unwrap();
    store::test_support::publish_fixture_file(
        &store,
        &store::test_support::FixtureFile {
            media_id: hidden,
            path: path.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    store
        .add_collection_item(&f.collection, &hidden.to_string())
        .unwrap();
    let (page, _) = f
        .get(&format!(
            "/api/v1/collections/{}/items?limit=1",
            f.collection
        ))
        .await;
    assert_eq!(page["data"]["total"], 1);
    assert_eq!(page["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        page["data"]["items"][0]["media_item_id"],
        visible.to_string()
    );
    assert!(page["data"]["next_cursor"].is_null());
    let (summary, _) = f.get("/api/v1/collections").await;
    assert_eq!(summary["data"][0]["item_count"], 1);
}

fn add_title_job(fixture: &Fixture, n: u64, extra: Option<Value>) {
    let id = fixture.add(n);
    let payload = extra.unwrap_or_else(|| serde_json::json!({ "media_id": id }));
    fixture
        .state
        .jobs()
        .lock()
        .upsert_def(jobs::NewDef {
            kind: jobs::JobKind::CatalogRefresh,
            name: format!("catalog {n}"),
            enabled: true,
            schedule: None,
            payload: payload.to_string(),
            timeout_secs: None,
            concurrency_key: None,
        })
        .unwrap();
}

#[tokio::test]
async fn actual_jobs_selected_title_statement_count_is_constant_for_five_and_fifty_defs() {
    let fixture = Fixture::new();
    for n in 1..=5 {
        add_title_job(&fixture, n, None);
    }
    let (small_body, small) = fixture.get("/api/v1/jobs").await;
    for n in 1..=5 {
        assert!(
            small_body["data"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["friendly_name"] == format!("刷新元数据《Movie {n}》"))
        );
    }
    for n in 6..=50 {
        add_title_job(&fixture, n, None);
    }
    let (large_body, large) = fixture.get("/api/v1/jobs").await;
    for n in 1..=50 {
        assert!(
            large_body["data"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["friendly_name"] == format!("刷新元数据《Movie {n}》"))
        );
    }
    assert_eq!(
        large.statements, small.statements,
        "actual jobs media title N+1: {small:?} -> {large:?}"
    );
}

#[tokio::test]
async fn jobs_invalid_subscribe_reference_does_not_fall_back_to_direct_media_title() {
    let fixture = Fixture::new();
    let media = fixture.add(1);
    add_title_job(
        &fixture,
        2,
        Some(serde_json::json!({
            "subscribe_id": "invalid", "media_id": media
        })),
    );
    let (body, _) = fixture.get("/api/v1/jobs").await;
    let row = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "catalog 2")
        .unwrap();
    assert_eq!(row["friendly_name"], "catalog 2");
}

#[tokio::test]
async fn jobs_subscription_title_wins_over_payload_media_and_missing_subscription_stays_raw() {
    let fixture = Fixture::new();
    let subscribed_media = fixture.add(1);
    let direct_media = fixture.add(2);
    let subscription = domain::SubscribeId::new();
    let store = fixture.state.store();
    store
        .insert_subscribe(&domain::Subscribe {
            id: subscription,
            user_id: store.user_id_by_token(TOKEN).unwrap().unwrap(),
            media_id: subscribed_media,
            coverage: domain::Coverage::Movie,
            fetch_mode: domain::FetchMode::Search,
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
    add_title_job(
        &fixture,
        3,
        Some(serde_json::json!({
            "subscribe_id": subscription, "media_id": direct_media
        })),
    );
    add_title_job(
        &fixture,
        4,
        Some(serde_json::json!({
            "subscribe_id": domain::SubscribeId::new(), "media_id": direct_media
        })),
    );
    let (body, _) = fixture.get("/api/v1/jobs").await;
    let rows = body["data"].as_array().unwrap();
    let name = |n| {
        rows.iter()
            .find(|row| row["name"] == format!("catalog {n}"))
            .unwrap()["friendly_name"]
            .as_str()
            .unwrap()
    };
    assert_eq!(name(3), "刷新元数据《Movie 1》");
    assert_eq!(name(4), "catalog 4");
}

fn add_missing_file(fixture: &Fixture, n: u64) {
    let media = fixture.add(n);
    let store = fixture.state.store();
    let coverage = store.file_coverage_for_media(media).unwrap().remove(0);
    let mut observation = store
        .watcher_presence_page(coverage.file.path.to_str().unwrap(), None, 1)
        .unwrap();
    assert_eq!(observation.len(), 1);
    observation[0].presence = domain::FilePresence::Missing;
    store
        .apply_ledger_presence(&observation, store::LedgerPresenceMode::Verify, 100)
        .unwrap();
}

#[tokio::test]
async fn actual_missing_route_keeps_complete_array_without_per_file_sql_growth() {
    let fixture = Fixture::new();
    for n in 1..=5 {
        add_missing_file(&fixture, n);
    }
    let uri = format!("/api/v1/libraries/{}/missing", fixture.library);
    let (small_body, small) = fixture.get(&uri).await;
    assert_eq!(small_body["data"].as_array().unwrap().len(), 5);
    for n in 6..=50 {
        add_missing_file(&fixture, n);
    }
    let (large_body, large) = fixture.get(&uri).await;
    let rows = large_body["data"].as_array().unwrap();
    assert_eq!(rows.len(), 50);
    for row in rows {
        assert_eq!(row["kind"], "movie");
        assert_eq!(row["files"].as_array().unwrap().len(), 1);
        assert_eq!(row["files"][0]["season_number"], 0);
        assert_eq!(row["files"][0]["episode_number"], 0);
        assert_eq!(row["files"][0]["size_bytes"], 0);
    }
    assert_eq!(
        large.statements, small.statements,
        "actual missing N+1: {small:?} -> {large:?}"
    );
}

#[tokio::test]
async fn actual_missing_route_keeps_normalized_path_identity_and_original_display_spelling() {
    let fixture = Fixture::new();
    let media = fixture.add(1);
    let path = fixture.dir.path().join("movies").join("cafe\u{0301}.mkv");
    std::fs::write(&path, b"decomposed file name").unwrap();
    let store = fixture.state.store();
    let file = store::test_support::publish_fixture_file(
        &store,
        &store::test_support::FixtureFile {
            media_id: media,
            path: path.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    let mut observation = store
        .watcher_presence_page(path.to_str().unwrap(), None, 1)
        .unwrap();
    assert_eq!(observation.len(), 1);
    assert_eq!(observation[0].file.file.file_id, file.file_id);
    observation[0].presence = domain::FilePresence::Missing;
    store
        .apply_ledger_presence(&observation, store::LedgerPresenceMode::Verify, 100)
        .unwrap();
    let (body, _) = fixture
        .get(&format!("/api/v1/libraries/{}/missing", fixture.library))
        .await;
    let rows = body["data"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        1,
        "physical missing identity must not depend on display spelling: {body}"
    );
    assert_eq!(rows[0]["files"][0]["id"], file.file_id.to_string());
    assert_eq!(rows[0]["files"][0]["file_path"], path.display().to_string());
}
