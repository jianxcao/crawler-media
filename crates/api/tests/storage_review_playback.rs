//! Public HTTP regression for the storage review's F1: a scoped `played` assertion
//! must reach every self/Web view, and only through the effective-state reader.
use axum::http::StatusCode;
use domain::{Media, MediaId, MediaKind, UnitKey, UserId};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{collections::HashMap, path::Path, sync::Arc};
use store::test_support::{FixtureFile, publish_fixture_file};
use tower::ServiceExt;
#[path = "management/common.rs"]
mod common;

struct Fixture {
    tmp: tempfile::TempDir,
    state: api::ApiState,
    app: axum::Router,
    media_id: MediaId,
    library: String,
}

impl Fixture {
    /// A TV Media owning S01E01–E02 in the default TV library.
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let state = common::state(
            tmp.path(),
            Arc::new(common::Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        );
        let app = api::router(state.clone());
        let media_id = MediaId::new();
        let root = tmp.path().join("data/library/tv/Test Work");
        std::fs::create_dir_all(&root).unwrap();
        let library;
        {
            let handle = state.store();
            let store = handle.as_ref();
            store
                .insert_media(&Media {
                    id: media_id,
                    kind: MediaKind::Tv,
                    title: "Test Work".into(),
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
            library = store.default_library(MediaKind::Tv).unwrap().unwrap().id;
            for episode in 1..=2 {
                publish_episode(store, media_id, &root, 1, episode);
            }
        }
        Self {
            tmp,
            state,
            app,
            media_id,
            library,
        }
    }

    async fn get(&self, path: &str) -> Value {
        let response = self
            .app
            .clone()
            .oneshot(common::request(
                "GET",
                path,
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        common::json_data(response).await
    }

    async fn post_mark(&self, body: Value) {
        let response = self
            .app
            .clone()
            .oneshot(common::request(
                "POST",
                "/api/v1/playback/marks",
                Some("management-secret"),
                body,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// Mark the whole item, one season, or one episode as played/unplayed.
    async fn mark(&self, season: i32, episode: i32, played: bool) {
        let media_item_id = self.media_id;
        let mut body = json!({"media_item_id": media_item_id, "played": played});
        if season >= 0 {
            body["season_number"] = json!(season);
        }
        if episode >= 0 {
            body["episode_number"] = json!(episode);
        }
        self.post_mark(body).await;
    }

    async fn season_marks(&self) -> Value {
        let media_item_id = self.media_id;
        self.get(&format!(
            "/api/v1/playback/marks?media_item_id={media_item_id}&season_number=1"
        ))
        .await
    }

    async fn episode_marks(&self, season: i32, episode: i32) -> Value {
        let media_item_id = self.media_id;
        self.get(&format!(
            "/api/v1/playback/marks?media_item_id={media_item_id}&season_number={season}&episode_number={episode}"
        ))
        .await
    }

    async fn episodes(&self, season: u32) -> Value {
        let media_item_id = self.media_id;
        self.get(&format!(
            "/api/v1/playback/items/{media_item_id}/episodes?season_number={season}"
        ))
        .await
    }

    async fn library_episodes(&self, season: u32) -> Value {
        let media_item_id = self.media_id;
        let library = &self.library;
        self.get(&format!(
            "/api/v1/libraries/{library}/items/{media_item_id}/episodes?season={season}"
        ))
        .await
    }

    async fn resume(&self, season: i32, episode: i32) -> Value {
        let media_item_id = self.media_id;
        self.get(&format!(
            "/api/v1/playback/resume?media_item_id={media_item_id}&season_number={season}&episode_number={episode}"
        ))
        .await
    }

    async fn up_next(&self) -> Value {
        self.get("/api/v1/playback/up-next").await
    }

    /// Drive the Store's own public reader, bypassing the HTTP views.
    fn effective_played(&self, user: UserId, season: i32, episode: i32) -> bool {
        let handle = self.state.store();
        let store = handle.as_ref();
        store
            .effective_unit_state(user, self.media_id, UnitKey::new(season, episode))
            .unwrap()
            .is_some_and(|state| state.played)
    }

    fn admin(&self) -> UserId {
        let handle = self.state.store();
        let store = handle.as_ref();
        store.admin_user_id().unwrap()
    }
}

fn publish_episode(
    store: &store::Store,
    media_id: MediaId,
    root: &Path,
    season: u32,
    episode: u32,
) {
    let path = root.join(format!("Work S{season:02}E{episode:02}.mkv"));
    std::fs::write(&path, b"video").unwrap();
    publish_fixture_file(
        store,
        &FixtureFile {
            media_id,
            path: path.to_string_lossy().to_string(),
            season: Some(season),
            episode: Some(episode),
            ..Default::default()
        },
    )
    .unwrap();
}

#[tokio::test]
async fn season_mark_is_visible_in_playback_views() {
    let fixture = Fixture::new();
    assert_eq!(fixture.season_marks().await["unplayed_count"], 2);
    fixture.mark(1, -1, true).await;
    assert_eq!(fixture.episode_marks(1, 1).await["played"], true);
    assert_eq!(fixture.episodes(1).await["episodes"][0]["played"], true);
    assert_eq!(fixture.library_episodes(1).await[0]["played"], true);
    let resume = fixture.resume(1, 1).await;
    assert_eq!(resume["played"], true);
    assert_eq!(resume["resume_status"], "already_played");
    assert!(
        fixture.up_next().await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(fixture.season_marks().await["unplayed_count"], Value::Null);
}

#[tokio::test]
async fn whole_item_mark_reaches_every_season_of_the_item() {
    let fixture = Fixture::new();
    // A second season proves the assertion is not scoped to one season.
    {
        let handle = fixture.state.store();
        let store = handle.as_ref();
        let root = fixture.tmp.path().join("data/library/tv/Test Work");
        publish_episode(store, fixture.media_id, &root, 2, 1);
    }
    fixture.mark(-1, -1, true).await;
    assert_eq!(fixture.episodes(1).await["episodes"][0]["played"], true);
    assert_eq!(fixture.episodes(2).await["episodes"][0]["played"], true);
    assert_eq!(fixture.episode_marks(2, 1).await["played"], true);
    assert!(
        fixture.up_next().await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn newer_episode_assertion_cancels_a_season_mark() {
    let fixture = Fixture::new();
    fixture.mark(1, -1, true).await;
    fixture.mark(1, 1, false).await;
    assert_eq!(fixture.episodes(1).await["episodes"][0]["played"], false);
    assert_eq!(fixture.episodes(1).await["episodes"][1]["played"], true);
    let up_next = fixture.up_next().await;
    assert_eq!(up_next["items"][0]["episode_number"], 1);
}

#[tokio::test]
async fn missing_season_is_empty_and_other_seasons_keep_their_states() {
    let fixture = Fixture::new();
    fixture.mark(1, -1, true).await;
    assert!(
        fixture.episodes(9).await["episodes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .library_episodes(9)
            .await
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(fixture.episodes(1).await["episodes"][0]["played"], true);
    assert_eq!(fixture.library_episodes(1).await[0]["played"], true);
}

#[tokio::test]
async fn newer_season_mark_overrides_an_older_episode_assertion() {
    let fixture = Fixture::new();
    fixture.mark(1, 1, true).await;
    fixture.mark(1, -1, false).await;
    assert_eq!(fixture.episodes(1).await["episodes"][0]["played"], false);
    assert_eq!(fixture.episode_marks(1, 1).await["played"], false);
}

#[tokio::test]
async fn equal_change_time_resolves_to_the_episode_scope() {
    let fixture = Fixture::new();
    let user = fixture.admin();
    {
        let handle = fixture.state.store();
        let store = handle.as_ref();
        // Both assertions carry the same change time; the more specific scope wins.
        for (season, episode, played) in [(1, -1, true), (1, 1, false)] {
            let change =
                store::playback::PlaybackMutation::marks(user, season, episode, Some(played), None);
            store
                .mutate_playback(fixture.media_id, &change, 5_000)
                .unwrap();
        }
    }
    assert_eq!(fixture.episode_marks(1, 1).await["played"], false);
    assert_eq!(fixture.episode_marks(1, 2).await["played"], true);
    assert!(fixture.effective_played(user, 1, 2));
}

#[tokio::test]
async fn another_user_keeps_their_own_watch_state() {
    let fixture = Fixture::new();
    fixture.mark(1, -1, true).await;
    let admin = fixture.admin();
    let member = UserId::new();
    {
        let handle = fixture.state.store();
        let store = handle.as_ref();
        store
            .insert_user(&domain::User {
                id: member,
                login: "member".into(),
                enabled: true,
                role: domain::UserRole::Member,
            })
            .unwrap();
    }
    assert!(fixture.effective_played(admin, 1, 1));
    assert!(!fixture.effective_played(member, 1, 1));
}

#[tokio::test]
async fn effective_state_survives_reopening_the_store() {
    let fixture = Fixture::new();
    fixture.mark(1, -1, true).await;
    let data = fixture.tmp.path().join("data");
    let reopened = store::Store::open(data).unwrap();
    let user = reopened.admin_user_id().unwrap();
    let state = reopened
        .effective_unit_state(user, fixture.media_id, UnitKey::new(1, 2))
        .unwrap();
    assert!(state.is_some_and(|state| state.played));
}
