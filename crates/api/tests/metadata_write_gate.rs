//! A closed write gate rejects mutations before validation or side effects.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use api::{ApiState, router};
use axum::http::{StatusCode, header};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

#[path = "management/common.rs"]
mod common;
use common::{Fixtures, json_body, request};

#[path = "metadata_write_gate/search_reads.rs"]
mod search_reads;

const TOKEN: &str = "management-secret";
const CACHE_KEY: &str = "/movie/42";
const CACHE_BODY: &str = r#"{"id":42,"title":"Gate fixture"}"#;

struct Fixture {
    tmp: tempfile::TempDir,
    state: ApiState,
    library_id: String,
    media_id: domain::MediaId,
    file_id: domain::LedgerId,
    path: PathBuf,
    history_key: String,
    presets_key: String,
}

impl Fixture {
    // Build before entering Tokio so background recovery cannot race assertions.
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let state = common::state(
            tmp.path(),
            Arc::new(Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::from([("search", common::nexusphp())]),
            }),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )
        .with_catalog(Arc::new(api::catalog::EmptyCatalog));
        let store = state.store();
        let guard = store.lock();
        let library = guard
            .default_library(domain::MediaKind::Movie)
            .unwrap()
            .unwrap();
        let media_id = domain::MediaId::new();
        guard
            .insert_media(&domain::Media {
                id: media_id,
                kind: domain::MediaKind::Movie,
                title: "Gate fixture".into(),
                year: None,
                original_title: None,
                aliases: Vec::new(),
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        std::fs::create_dir_all(&library.root_paths[0]).unwrap();
        let path = library.root_paths[0].join("Gate.fixture.mkv");
        std::fs::write(&path, b"published video").unwrap();
        let file = store::test_support::publish_fixture_file(
            &guard,
            &store::test_support::FixtureFile {
                media_id,
                path: path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        guard
            .put_catalog_cache("tmdb", CACHE_KEY, CACHE_BODY, 1, None)
            .unwrap();
        let user = guard.list_users().unwrap().into_iter().next().unwrap();
        let history_key = format!("user.{}.search.history", user.id);
        let presets_key = format!("user.{}.search.presets", user.id);
        guard
            .put_setting(&history_key, r#"[{"id":"gate-history","query":"fixture"}]"#)
            .unwrap();
        guard.put_setting(&presets_key, r#"["original"]"#).unwrap();
        guard
            .insert_search_snapshot("gate-history", "torrents", "fixture", "{}", 1)
            .unwrap();
        guard
            .insert_metric(
                user.id,
                media_id,
                0,
                Some("fixture"),
                None,
                0,
                0,
                0,
                None,
                None,
                10,
                1,
            )
            .unwrap();
        drop(guard);
        Self {
            tmp,
            state,
            library_id: library.id,
            media_id,
            file_id: file.file_id,
            path,
            history_key,
            presets_key,
        }
    }

    fn assert_unchanged(&self) {
        let store = self.state.store();
        let guard = store.lock();
        let file = guard.file_coverage_by_id(self.file_id).unwrap().unwrap();
        assert_eq!(file.file.media_id, self.media_id);
        assert_eq!(file.file.path, self.path);
        assert!(guard.list_pending_file_deletions().unwrap().is_empty());
        assert_eq!(
            guard
                .get_catalog_cache("tmdb", CACHE_KEY)
                .unwrap()
                .as_deref(),
            Some(CACHE_BODY)
        );
        assert!(guard.get_search_snapshot("gate-history").unwrap().is_some());
        assert_eq!(
            guard.get_setting(&self.history_key).unwrap().as_deref(),
            Some(r#"[{"id":"gate-history","query":"fixture"}]"#)
        );
        assert_eq!(
            guard.get_setting(&self.presets_key).unwrap().as_deref(),
            Some(r#"["original"]"#)
        );
        assert_eq!(std::fs::read(&self.path).unwrap(), b"published video");
        let db = rusqlite::Connection::open(self.tmp.path().join("data/library.db")).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM playback_metrics", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    fn write_entries(&self) -> Vec<(&'static str, String)> {
        let lib = &self.library_id;
        let item = self.media_id;
        let file = self.file_id;
        vec![
            ("DELETE", format!("/ledger/{file}?delete_file=true")),
            ("POST", format!("/libraries/{lib}/metadata/refresh")),
            (
                "POST",
                format!("/libraries/{lib}/items/{item}/metadata/refresh"),
            ),
            ("POST", format!("/libraries/{lib}/items/{item}/probe")),
            ("POST", format!("/libraries/{lib}/items/relax-filter")),
            ("POST", "/reidentify".into()),
            ("POST", "/reidentify/extras".into()),
            (
                "POST",
                format!("/libraries/{lib}/items/{item}/chapters/generate"),
            ),
            (
                "POST",
                format!("/libraries/{lib}/items/{item}/chapters/refresh"),
            ),
            ("POST", format!("/libraries/{lib}/cover")),
            ("DELETE", format!("/libraries/{lib}/cover")),
            ("POST", format!("/libraries/{lib}/cover/generate")),
            (
                "POST",
                format!("/libraries/{lib}/items/{item}/artwork/select"),
            ),
            (
                "POST",
                format!("/libraries/{lib}/items/{item}/artwork/upload"),
            ),
            ("POST", "/unidentified/claim".into()),
            (
                "DELETE",
                "/catalog/cache?source=tmdb&cache_key=%2Fmovie%2F42".into(),
            ),
            ("PUT", format!("/media/{item}/metadata-overrides")),
            ("POST", format!("/ledger/{file}/retransfer")),
            ("DELETE", "/search/history/gate-history".into()),
            ("DELETE", "/search/history".into()),
            ("PUT", "/search/presets".into()),
            ("POST", "/playback/sessions".into()),
            ("POST", "/playback/progress".into()),
            ("POST", "/playback/metrics".into()),
            ("POST", "/playback/marks".into()),
            ("POST", "/playback/client-log".into()),
            ("DELETE", "/playback/history?scope=all".into()),
            ("POST", "/playback/activity/sessions/gate-device/end".into()),
            ("DELETE", "/playback/devices/gate-device".into()),
        ]
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

async fn assert_rejected(fixture: &Fixture, app: &axum::Router, readonly: bool) {
    let expected = if readonly {
        StatusCode::CONFLICT
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let code = if readonly {
        "instance.read_only"
    } else {
        "runtime.busy"
    };
    for (method, path) in fixture.write_entries() {
        let uri = format!("/api/v1{path}");
        let response = app
            .clone()
            .oneshot(request(
                method,
                &uri,
                Some(TOKEN),
                json!({
                    "library_file_id": fixture.file_id.to_string(),
                    "media_item_id": fixture.media_id.to_string(),
                    "watched_ms": 20,
                    "presets": ["replacement"],
                }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{method} {uri}");
        assert_eq!(
            response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|h| h.to_str().ok()),
            if readonly { None } else { Some("1") }
        );
        assert_eq!(
            json_body(response).await["error"]["code"],
            code,
            "{method} {uri}"
        );
        fixture.assert_unchanged();
    }
}

#[test]
fn metadata_search_and_playback_writes_reject_readonly_instance() {
    let fixture = Fixture::new();
    let app = router(fixture.state.clone().with_instance_writable(false));
    runtime().block_on(assert_rejected(&fixture, &app, true));
}

#[test]
fn metadata_search_and_playback_writes_reject_maintenance() {
    let fixture = Fixture::new();
    let gate = fixture.state.write_gate();
    let _closed = gate.close();
    let app = router(fixture.state.clone());
    runtime().block_on(assert_rejected(&fixture, &app, false));
}

#[test]
fn admitted_ledger_delete_removes_facts_and_bytes() {
    let fixture = Fixture::new();
    let app = router(fixture.state.clone());
    runtime().block_on(async {
        let response = app
            .oneshot(request(
                "DELETE",
                &format!("/api/v1/ledger/{}?delete_file=true", fixture.file_id),
                Some(TOKEN),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body(response).await["data"]["deleted"], true);
    });
    assert!(
        fixture
            .state
            .store()
            .lock()
            .file_coverage_by_id(fixture.file_id)
            .unwrap()
            .is_none()
    );
    assert!(!fixture.path.exists());
}

#[test]
fn reads_and_readonly_post_computations_remain_available() {
    let fixture = Fixture::new();
    let def = fixture
        .state
        .jobs()
        .lock()
        .list_defs()
        .unwrap()
        .into_iter()
        .find(|def| def.kind == jobs::JobKind::DatabaseMaintenance)
        .unwrap();
    let gate = fixture.state.write_gate();
    for readonly in [false, true] {
        let _closed = if readonly { None } else { Some(gate.close()) };
        let app = router(fixture.state.clone().with_instance_writable(!readonly));
        runtime().block_on(async {
            let lib = &fixture.library_id;
            let item = fixture.media_id;
            let entries = [
                ("GET", "/api/v1/ledger".into(), Value::Null),
                (
                    "GET",
                    format!("/api/v1/libraries/{lib}/items/{item}/reidentification-preview"),
                    Value::Null,
                ),
                (
                    "GET",
                    format!("/api/v1/libraries/{lib}/organize-preview"),
                    Value::Null,
                ),
                (
                    "GET",
                    format!("/api/v1/libraries/{lib}/path-reconciliation-preview"),
                    Value::Null,
                ),
                (
                    "GET",
                    format!("/api/v1/libraries/{lib}/root-consolidation-preview"),
                    Value::Null,
                ),
                (
                    "POST",
                    format!("/api/v1/jobs/{}/maintenance-preview", def.id),
                    json!({}),
                ),
                (
                    "POST",
                    "/api/v1/playback/decide".into(),
                        json!({"media_item_id": item.to_string(), "file_id": fixture.file_id.to_string()}),
                ),
                (
                    "POST",
                    "/api/v1/settings/scrape/preview-naming".into(),
                    json!({}),
                ),
                (
                    "POST",
                    "/api/v1/subscriptions/title-preview".into(),
                    json!({"title_ref": "tmdb:movie:1"}),
                ),
                (
                    "POST",
                    "/api/v1/subscriptions/download-routing-preview".into(),
                    json!({"kind": "movie", "library_id": lib, "title": "Gate fixture"}),
                ),
            ];
            for (method, uri, body) in entries {
                let response = app
                    .clone()
                    .oneshot(request(method, &uri, Some(TOKEN), body))
                    .await
                    .unwrap();
                let status = response.status();
                let body = json_body(response).await;
                assert_eq!(
                    status,
                    StatusCode::OK,
                    "{method} {uri}, readonly={readonly}: {body}"
                );
            }
        });
        fixture.assert_unchanged();
    }
}

#[test]
fn protocol_mutations_obey_the_same_write_admission() {
    for readonly in [true, false] {
        let fixture = Fixture::new();
        let gate = fixture.state.write_gate().clone();
        let closed = (!readonly).then(|| gate.close());
        let state = fixture.state.clone().with_instance_writable(!readonly);
        let app = router(state);
        let item = domain::TypedItemId::Work(fixture.media_id).to_wire_string();
        runtime().block_on(async {
            for prefix in ["", "/emby"] {
                for (method, path) in [
                    ("POST", format!("/UserFavoriteItems/{item}")),
                    ("DELETE", format!("/UserFavoriteItems/{item}")),
                    ("POST", format!("/UserPlayedItems/{item}")),
                    ("DELETE", format!("/UserPlayedItems/{item}")),
                    ("POST", format!("/Items/{item}/PlaybackInfo")),
                    ("POST", "/Sessions/Playing".into()),
                    ("POST", "/Sessions/Playing/Progress".into()),
                    ("POST", "/Sessions/Playing/Stopped".into()),
                    ("POST", "/DisplayPreferences/fixture".into()),
                    ("POST", "/Users/AuthenticateByName".into()),
                ] {
                    let response = app
                        .clone()
                        .oneshot(request(
                            method,
                            &format!("{prefix}{path}"),
                            Some(TOKEN),
                            json!({}),
                        ))
                        .await
                        .unwrap();
                    assert_eq!(
                        response.status(),
                        if readonly {
                            StatusCode::CONFLICT
                        } else {
                            StatusCode::SERVICE_UNAVAILABLE
                        },
                        "{method} {prefix}{path}"
                    );
                    if !readonly {
                        assert_eq!(response.headers()[header::RETRY_AFTER], "1");
                    }
                }
                let ping = app
                    .clone()
                    .oneshot(request(
                        "POST",
                        &format!("{prefix}/System/Ping"),
                        None,
                        Value::Null,
                    ))
                    .await
                    .unwrap();
                assert_eq!(ping.status(), StatusCode::OK);
                let info = app
                    .clone()
                    .oneshot(request(
                        "GET",
                        &format!("{prefix}/System/Info/Public"),
                        None,
                        Value::Null,
                    ))
                    .await
                    .unwrap();
                assert_eq!(info.status(), StatusCode::OK);
            }
        });
        fixture.assert_unchanged();
        let store = fixture.state.store();
        let store = store.lock();
        let user = store.list_users().unwrap().into_iter().next().unwrap();
        assert!(
            store
                .unit_rows(user.id, fixture.media_id)
                .unwrap()
                .is_empty()
        );
        drop(closed);
    }
}
