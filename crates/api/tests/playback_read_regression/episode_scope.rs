use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::{FilePresence, LibraryFile, LibraryId, Media, MediaId, MediaKind, UnitKey};
use serde_json::{Value, json};
use store::test_support::{FixtureFile, publish_fixture_file_units};
use tower::ServiceExt;

struct Episodes {
    _temp: tempfile::TempDir,
    state: api::ApiState,
    app: axum::Router,
    media: MediaId,
    library: LibraryId,
    root: std::path::PathBuf,
}

impl Episodes {
    fn new(access: &str) -> Self {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let state = super::common::make_state(temp.path());
        let root = temp.path().join("tv");
        std::fs::create_dir_all(&root).unwrap();
        let store = state.store();
        let library = store
            .create_library(
                MediaKind::Tv,
                "Episodes",
                &[root.to_str().unwrap()],
                access,
                true,
                &[],
            )
            .unwrap();
        let media = MediaId::new();
        store
            .insert_media(&Media {
                id: media,
                kind: MediaKind::Tv,
                title: "Episode scope".into(),
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
        drop(store);
        Self {
            _temp: temp,
            app: api::router(state.clone()),
            state,
            media,
            library: library.id.parse().unwrap(),
            root,
        }
    }

    fn publish(&self, name: &str, units: &[UnitKey]) -> LibraryFile {
        let path = self.root.join(name);
        std::fs::write(&path, b"episode").unwrap();
        publish_fixture_file_units(
            &self.state.store(),
            &FixtureFile {
                media_id: self.media,
                path: path.display().to_string(),
                ..Default::default()
            },
            units,
        )
        .unwrap()
    }

    fn presence(&self, file: &LibraryFile, presence: FilePresence) {
        let store = self.state.store();
        let mut coverage = store.file_coverage_by_id(file.file_id).unwrap().unwrap();
        coverage.file.presence = presence;
        store
            .publish_files(
                &store::library_write::WriteScope {
                    library_id: self.library,
                    config_revision: store
                        .library_config_revision(&self.library.to_string())
                        .unwrap(),
                },
                &[store::library_write::FilePublication {
                    file: &coverage.file,
                    units: &coverage.units,
                    expected_version: Some(&file.version),
                    nfo_observation: None,
                    probe_required: false,
                }],
                2_000,
            )
            .unwrap();
    }

    async fn request(&self, season: Option<u32>, token: &str) -> axum::response::Response {
        let query = season.map_or_else(String::new, |season| format!("?season_number={season}"));
        let request = Request::builder()
            .uri(format!(
                "/api/v1/playback/items/{}/episodes{query}",
                self.media
            ))
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        self.app.clone().oneshot(request).await.unwrap()
    }

    async fn list(&self, season: Option<u32>) -> Vec<Value> {
        let response = self.request(season, "management-secret").await;
        assert_eq!(response.status(), StatusCode::OK);
        super::common::json_data(response).await["episodes"]
            .as_array()
            .unwrap()
            .clone()
    }
}

#[tokio::test]
async fn episode_scope_excludes_missing_and_unknown_files() {
    let fixture = Episodes::new("everyone");
    let present = fixture.publish("present.mkv", &[UnitKey::new(1, 1)]);
    let missing = fixture.publish("missing.mkv", &[UnitKey::new(1, 2)]);
    let unknown = fixture.publish("unknown.mkv", &[UnitKey::new(1, 3)]);
    fixture.presence(&missing, FilePresence::Missing);
    fixture.presence(&unknown, FilePresence::Unknown);

    for season in [Some(1), None] {
        let rows = fixture.list(season).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["episode_number"], 1);
        assert_eq!(rows[0]["owned"], true);
        assert_eq!(rows[0]["file_ids"], json!([present.file_id]));
    }
}

#[tokio::test]
async fn episode_scope_keeps_only_the_playable_version() {
    let fixture = Episodes::new("everyone");
    let present = fixture.publish("present.mkv", &[UnitKey::new(1, 1)]);
    let missing = fixture.publish("missing-version.mkv", &[UnitKey::new(1, 1)]);
    fixture.presence(&missing, FilePresence::Missing);

    for season in [Some(1), None] {
        let rows = fixture.list(season).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["file_ids"], json!([present.file_id]));
    }
}

#[tokio::test]
async fn episode_scope_expands_a_shared_file_once_per_episode() {
    let fixture = Episodes::new("everyone");
    let shared = fixture.publish("double.mkv", &[UnitKey::new(1, 1), UnitKey::new(1, 2)]);

    for season in [Some(1), None] {
        let rows = fixture.list(season).await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["episode_number"], 1);
        assert_eq!(rows[1]["episode_number"], 2);
        for row in rows {
            assert_eq!(row["file_ids"], json!([shared.file_id]));
            assert_eq!(row["owned"], true);
        }
    }
}

#[tokio::test]
async fn episode_scope_preserves_season_zero_episode_zero() {
    let fixture = Episodes::new("everyone");
    let special = fixture.publish("S00E00.mkv", &[UnitKey::new(0, 0)]);

    for season in [Some(0), None] {
        let rows = fixture.list(season).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["season_number"], 0);
        assert_eq!(rows[0]["episode_number"], 0);
        assert_eq!(rows[0]["file_ids"], json!([special.file_id]));
    }
}

#[tokio::test]
async fn episode_scope_returns_empty_success_for_visible_unplayable_media() {
    let fixture = Episodes::new("everyone");
    let missing = fixture.publish("missing.mkv", &[UnitKey::new(1, 1)]);
    let unknown = fixture.publish("unknown.mkv", &[UnitKey::new(1, 2)]);
    fixture.presence(&missing, FilePresence::Missing);
    fixture.presence(&unknown, FilePresence::Unknown);

    for season in [Some(1), None] {
        assert!(fixture.list(season).await.is_empty());
    }
}

#[tokio::test]
async fn episode_scope_hides_media_outside_the_members_visible_libraries() {
    let fixture = Episodes::new("selected");
    fixture.publish("hidden.mkv", &[UnitKey::new(1, 1)]);
    let (_, token) = super::common::seed_member(&fixture.state.store(), "episode-member");
    assert_eq!(fixture.list(Some(1)).await.len(), 1);

    for season in [Some(1), None] {
        let response = fixture.request(season, &token).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["code"], "playback.item_missing");
    }
}
