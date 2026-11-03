#[allow(dead_code)]
#[path = "playback_read_regression/common.rs"]
mod common;

use std::sync::Arc;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use domain::{LibraryId, MediaId, UserId};
use serde_json::Value;
use tower::ServiceExt;

struct Fixture {
    _tmp: tempfile::TempDir,
    state: api::ApiState,
    library: LibraryId,
    user: UserId,
    token: String,
    media: Vec<MediaId>,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let state = common::make_state(tmp.path());
        let store = state.store();
        let root = common::seed_library(&store, tmp.path(), "integrity-reads");
        let library = store
            .library_for_path_strict(&root, domain::MediaKind::Movie)
            .unwrap()
            .unwrap()
            .id
            .parse()
            .unwrap();
        let (user, token) = common::seed_member(&store, "integrity-reader");
        let media = (0..3)
            .map(|n| {
                let name = format!("Original {n}");
                let media = common::seed_media(&store, &name, Some(2024));
                common::add_ledger(&store, media, &root, &name);
                common::favorite(&store, user, media, true);
                media
            })
            .collect();
        Self {
            _tmp: tmp,
            state,
            library,
            user,
            token,
            media,
        }
    }

    fn partially_repair(&self) {
        let store = self.state.store();
        let db = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
        db.execute(
            "UPDATE library_items SET title='Broken',title_sort='Broken',size_bytes=9999
             WHERE library_id=?1",
            [self.library.to_string()],
        )
        .unwrap();
        store
            .begin_library_integrity(self.library, true, "read-owner", 0)
            .unwrap();
        for step in 1..10_000 {
            let status = store
                .advance_library_operation("read-owner", step, 1)
                .unwrap();
            let repaired: usize = db
                .query_row(
                    "SELECT count(*) FROM library_items WHERE library_id=?1 AND title<>'Broken'",
                    [self.library.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            if repaired > 0 && repaired < 3 {
                assert_eq!(status.state, "applying");
                return;
            }
            assert_eq!(
                status.state, "applying",
                "repair must yield after a partial publication"
            );
        }
        panic!("no partial repair was observable");
    }

    fn finish(&self) {
        let store = self.state.store();
        for step in 10_000..30_000 {
            let status = store
                .advance_library_operation("read-owner", step, 1)
                .unwrap();
            if status.state == "succeeded" {
                return;
            }
            assert_eq!(status.state, "applying");
        }
        panic!("repair did not finish");
    }

    async fn get(&self, uri: &str, token: &str) -> (StatusCode, Value) {
        let response = api::router(self.state.clone())
            .oneshot(
                Request::get(uri)
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
}

#[tokio::test]
async fn favorites_reject_partial_repairs_without_returning_mixed_sort_or_total() {
    let fixture = Fixture::new();
    fixture.partially_repair();
    for uri in [
        "/api/v1/playback/favorites?sort=title&unwatched_first=true",
        "/api/v1/playback/favorites?sort=favorited_at",
        "/api/v1/playback/favorites/position?start=0",
    ] {
        let (status, body) = fixture.get(uri, &fixture.token).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{uri}: {body}");
        assert_eq!(body["error"]["code"], "library.reindexing");
    }
    fixture.finish();
    let (status, body) = fixture
        .get(
            "/api/v1/playback/favorites?sort=title&order=asc&unwatched_first=true",
            &fixture.token,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["total"], 3);
    let ids = body["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            row["media_item_id"]
                .as_str()
                .unwrap()
                .parse::<MediaId>()
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(ids, fixture.media);
}

#[tokio::test]
async fn roster_rejects_partial_repairs_instead_of_claiming_fresh_statistics() {
    let fixture = Fixture::new();
    fixture.partially_repair();
    for (uri, token) in [
        ("/api/v1/libraries?scope=admin", "management-secret"),
        ("/api/v1/libraries", fixture.token.as_str()),
    ] {
        let (status, body) = fixture.get(uri, token).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{uri}: {body}");
        assert_eq!(body["error"]["code"], "library.reindexing");
    }
    fixture.finish();
    assert_eq!(
        fixture
            .get("/api/v1/libraries?scope=admin", "management-secret")
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn library_config_write_preserves_the_frozen_admission_error() {
    let fixture = Fixture::new();
    fixture.partially_repair();
    let response = api::router(fixture.state.clone())
        .oneshot(
            Request::put(format!("/api/v1/libraries/{}/default", fixture.library))
                .header("authorization", "Bearer management-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "library.reindexing");
}

#[tokio::test]
async fn favorites_keep_the_completed_integrity_failure_reason() {
    let fixture = Fixture::new();
    let store = fixture.state.store();
    let db = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
    db.execute(
        "UPDATE library_items SET title='Corrupt' WHERE library_id=?1",
        [fixture.library.to_string()],
    )
    .unwrap();
    store
        .begin_library_integrity(fixture.library, false, "read-owner", 0)
        .unwrap();
    fixture.finish();
    assert!(
        store
            .library_integrity_report("read-owner")
            .unwrap()
            .has_issues()
    );
    let (status, body) = fixture
        .get("/api/v1/playback/favorites?sort=title", &fixture.token)
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("library.integrity"),
        "{body}"
    );
}

#[tokio::test]
async fn unauthorized_frozen_library_does_not_block_visible_favorites_or_roster() {
    let fixture = Fixture::new();
    let store = fixture.state.store();
    let hidden = store
        .create_library(
            domain::MediaKind::Movie,
            "Hidden repair",
            &["/hidden-integrity-reads"],
            "selected",
            true,
            &[],
        )
        .unwrap();
    store
        .begin_library_integrity(hidden.id.parse().unwrap(), true, "hidden-owner", 0)
        .unwrap();
    for uri in [
        "/api/v1/playback/favorites?sort=title",
        "/api/v1/libraries",
        "/Items",
    ] {
        let (status, body) = fixture.get(uri, &fixture.token).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    }
    let (status, _) = fixture
        .get(
            &format!("/api/v1/libraries/{}/items", hidden.id),
            &fixture.token,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[test]
fn user_removal_keeps_disabled_user_and_cleaning_record_until_integrity_finishes() {
    let fixture = Fixture::new();
    fixture.partially_repair();
    let service = api::user_removal::UserRemovalService::new(Arc::new(fixture.state.clone()));
    let accepted = service.accept(fixture.user).unwrap();
    assert_eq!(accepted.phase, "cleaning");
    assert!(service.continue_removal(fixture.user).is_err());
    let store = fixture.state.store();
    assert!(!store.get_user(fixture.user).unwrap().unwrap().enabled);
    assert!(store.user_id_by_token(&fixture.token).unwrap().is_none());
    let pending = api::user_removal::pending_removal_of(&store, fixture.user)
        .unwrap()
        .unwrap();
    assert_eq!(pending.phase, "cleaning");
    assert_eq!(api::user_removal::status_dto(&pending)["state"], "failed");
    assert!(
        store
            .unit_state(fixture.user, fixture.media[0], -1, -1)
            .unwrap()
            .is_some()
    );
    fixture.finish();
    drop(service);
    drop(store);
    let Fixture {
        _tmp,
        state,
        library,
        user,
        media,
        ..
    } = fixture;
    drop(state);
    let reopened = common::make_state(_tmp.path());
    let service = api::user_removal::UserRemovalService::new(Arc::new(reopened.clone()));
    service.recover_pending_removals().unwrap();
    let store = reopened.store();
    assert!(store.get_user(user).unwrap().is_none());
    for media in media {
        assert!(store.unit_rows(user, media).unwrap().is_empty());
        assert!(
            store
                .get_user_library_summary(user, library, media)
                .unwrap()
                .is_none()
        );
    }
}
