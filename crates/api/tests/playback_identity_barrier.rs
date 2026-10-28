#[path = "management/common.rs"]
mod common;

use std::{collections::HashMap, sync::Arc};

use axum::http::StatusCode;
use domain::{Media, MediaId, MediaKind, User, UserId, UserRole};
use downloader::MemoryDownloader;
use serde_json::json;
use store::playback::MediaMergeRequest;
use store::test_support::{FixtureFile, publish_fixture_file};
use tower::ServiceExt;

struct Fixture {
    _tmp: tempfile::TempDir,
    state: api::ApiState,
    app: axum::Router,
    user: UserId,
    survivor: MediaId,
    loser: MediaId,
}

impl Fixture {
    // Construct outside Tokio so unrelated background drivers never race the merge.
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let state = common::state(
            tmp.path(),
            Arc::new(common::Fixtures {
                requests: Default::default(),
                bodies: HashMap::new(),
            }),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        );
        let user = UserId::new();
        let survivor = MediaId::new();
        let loser = MediaId::new();
        {
            let shared = state.store();
            let store = shared.lock();
            store
                .insert_user_with_password(
                    &User {
                        id: user,
                        login: "marks-user".into(),
                        enabled: true,
                        role: UserRole::Admin,
                    },
                    "password",
                )
                .unwrap();
            store.set_user_token(user, "marks-token").unwrap();
            for (id, title) in [(survivor, "Survivor"), (loser, "Loser")] {
                store
                    .insert_media(&Media {
                        id,
                        kind: MediaKind::Movie,
                        title: title.into(),
                        year: Some(2024),
                        original_title: None,
                        aliases: vec![],
                        tmdb_id: None,
                        douban_id: None,
                        tvdb_id: None,
                        bangumi_id: None,
                        anilist_id: None,
                    })
                    .unwrap();
                publish_fixture_file(
                    &store,
                    &FixtureFile {
                        media_id: id,
                        path: tmp
                            .path()
                            .join(format!("data/library/movies/{title}.mkv"))
                            .display()
                            .to_string(),
                        ..Default::default()
                    },
                )
                .unwrap();
            }
        }
        let app = api::router(state.clone());
        Self {
            _tmp: tmp,
            state,
            app,
            user,
            survivor,
            loser,
        }
    }

    fn begin(&self) -> String {
        self.state
            .store()
            .lock()
            .begin_media_merge(
                &MediaMergeRequest {
                    request_id: "00000000-0000-0000-0001-0000000000a1".into(),
                    survivor: self.survivor,
                    loser: self.loser,
                    survivor_revision: 0,
                    loser_revision: 0,
                    refresh_targets: Default::default(),
                },
                1_000,
            )
            .unwrap()
    }

    async fn mark(&self, path: &str, body: serde_json::Value) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(common::request("POST", path, Some("marks-token"), body))
            .await
            .unwrap()
    }

    fn assert_no_facts(&self) {
        let shared = self.state.store();
        let store = shared.lock();
        assert!(
            store
                .unit_rows(self.user, self.survivor)
                .unwrap()
                .is_empty()
        );
        assert!(store.unit_rows(self.user, self.loser).unwrap().is_empty());
        let library = store.default_library(MediaKind::Movie).unwrap().unwrap();
        for media in [self.survivor, self.loser] {
            assert!(
                store
                    .get_user_library_summary(self.user, library.id.parse().unwrap(), media,)
                    .unwrap()
                    .is_none()
            );
        }
    }
}

#[test]
fn self_marks_report_frozen_and_retired_conflicts_without_redirecting_the_write() {
    let f = Fixture::new();
    let operation = f.begin();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for target in [f.survivor, f.loser] {
            let response = f
                .mark(
                    "/api/v1/playback/marks",
                    json!({"media_item_id":target,"favorite":true}),
                )
                .await;
            assert_eq!(response.status(), StatusCode::CONFLICT);
            assert_eq!(
                common::json_error(response).await["code"],
                "playback.protected"
            );
        }
        f.assert_no_facts();
        assert_eq!(
            f.state
                .store()
                .lock()
                .advance_media_merge(&operation, 1_200)
                .unwrap()
                .state,
            "succeeded"
        );
        let response = f
            .mark(
                "/api/v1/playback/marks",
                json!({"media_item_id":f.loser,"played":true}),
            )
            .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert!(
            common::json_error(response).await["message"]
                .as_str()
                .unwrap()
                .contains(&f.survivor.to_string())
        );
        f.assert_no_facts();
        let response = f
            .mark(
                "/api/v1/playback/marks",
                json!({"media_item_id":f.survivor,"favorite":true,"played":true}),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let unit = f
            .state
            .store()
            .lock()
            .unit_state(f.user, f.survivor, -1, -1)
            .unwrap()
            .unwrap();
        assert!(unit.played && unit.favorite);
    });
}

#[test]
fn jellyfin_marks_refuse_frozen_identity_and_retired_item_without_new_facts() {
    let f = Fixture::new();
    let operation = f.begin();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for target in [f.survivor, f.loser] {
            for mark in ["FavoriteItems", "PlayedItems"] {
                let id = domain::TypedItemId::Work(target).to_wire_string();
                let response = f.mark(&format!("/User{mark}/{id}"), json!({})).await;
                assert_eq!(response.status(), StatusCode::CONFLICT, "{target} {mark}");
            }
        }
        f.assert_no_facts();
        assert_eq!(
            f.state
                .store()
                .lock()
                .advance_media_merge(&operation, 1_200)
                .unwrap()
                .state,
            "succeeded"
        );
        let id = domain::TypedItemId::Work(f.loser).to_wire_string();
        assert_eq!(
            f.mark(&format!("/UserFavoriteItems/{id}"), json!({}))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        f.assert_no_facts();
        let id = domain::TypedItemId::Work(f.survivor).to_wire_string();
        assert_eq!(
            f.mark(&format!("/UserFavoriteItems/{id}"), json!({}))
                .await
                .status(),
            StatusCode::OK
        );
        assert!(
            f.state
                .store()
                .lock()
                .unit_state(f.user, f.survivor, -1, -1)
                .unwrap()
                .unwrap()
                .favorite
        );
    });
}
