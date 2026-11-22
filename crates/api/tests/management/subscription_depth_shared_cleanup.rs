use api::Store;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;
use super::subscription_depth::authed_app;
use store::test_support::{FixtureFile, publish_fixture_file, publish_fixture_file_units};

struct SharedFixture {
    sub_a: String,
    sub_b: String,
    media_id: String,
    s1e1: std::path::PathBuf,
    s2e1: std::path::PathBuf,
    shared: std::path::PathBuf,
}

async fn setup_shared_multiseason_fixture(
    app: &axum::Router,
    tmp: &tempfile::TempDir,
) -> SharedFixture {
    let sub_a = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "tv", "title": "Shared Show", "tmdb_id": "200" },
                    "coverage": { "kind": "tv", "season": 2, "episode_from": 1, "episode_to": 2 },
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let sub_b = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "tv", "title": "Shared Show", "tmdb_id": "200" },
                    "coverage": { "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2 },
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let media_id = sub_a["data"]["media"]["id"].as_str().unwrap().to_string();
    let media_uuid: domain::MediaId = media_id.parse().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let dir = tmp.path().join("data/library/tv");
    std::fs::create_dir_all(&dir).unwrap();
    let s1e1 = dir.join("s1e1.mkv");
    let s2e1 = dir.join("s2e1.mkv");
    let shared = dir.join("shared-s1e2-s2e2.mkv");
    for (path, season, episode) in [(&s1e1, 1u32, 1u32), (&s2e1, 2, 1)] {
        std::fs::write(path, b"ep").unwrap();
        publish_fixture_file(
            &store,
            &FixtureFile {
                file_id: Some(domain::LedgerId::new()),
                media_id: media_uuid,
                path: path.display().to_string(),
                season: Some(season),
                episode: Some(episode),
                ..Default::default()
            },
        )
        .unwrap();
    }
    std::fs::write(&shared, b"shared").unwrap();
    publish_fixture_file_units(
        &store,
        &FixtureFile {
            media_id: media_uuid,
            path: shared.display().to_string(),
            ..Default::default()
        },
        &[domain::UnitKey::new(1, 2), domain::UnitKey::new(2, 2)],
    )
    .unwrap();
    SharedFixture {
        sub_a: sub_a["data"]["id"].as_str().unwrap().to_string(),
        sub_b: sub_b["data"]["id"].as_str().unwrap().to_string(),
        media_id,
        s1e1,
        s2e1,
        shared,
    }
}

#[tokio::test]
async fn season_cleanup_preserves_shared_multiseason_file_owned_by_other_subscribe() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let f = setup_shared_multiseason_fixture(&app, &tmp).await;
    let store = Store::open(tmp.path().join("data")).unwrap();
    let sid_a: domain::SubscribeId = f.sub_a.parse().unwrap();
    let sid_b: domain::SubscribeId = f.sub_b.parse().unwrap();

    let mut facts_a = domain::SubscribeFacts::default();
    facts_a.upsert(
        Some(2),
        Some(1),
        domain::QualityFact {
            score: 80,
            path: Some(f.s2e1.display().to_string()),
        },
    );
    facts_a.upsert(
        Some(2),
        Some(2),
        domain::QualityFact {
            score: 80,
            path: Some(f.shared.display().to_string()),
        },
    );
    store.save_subscribe_facts(sid_a, &facts_a).unwrap();

    let mut facts_b = domain::SubscribeFacts::default();
    facts_b.upsert(
        Some(1),
        Some(1),
        domain::QualityFact {
            score: 80,
            path: Some(f.s1e1.display().to_string()),
        },
    );
    facts_b.upsert(
        Some(1),
        Some(2),
        domain::QualityFact {
            score: 80,
            path: Some(f.shared.display().to_string()),
        },
    );
    store.save_subscribe_facts(sid_b, &facts_b).unwrap();

    let resp = app
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{}/season-cleanup", f.sub_a),
            Some("management-secret"),
            json!({ "seasons": [2], "delete_library_files": true, "delete_torrents": false }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["data"]["binned"], 1, "{body}");
    assert!(!f.s2e1.exists(), "exclusively owned s2e1 must be deleted");
    assert!(
        f.shared.exists(),
        "shared multi-season file owned by Subscribe B must survive"
    );
    assert!(f.s1e1.exists());

    let rem_a = store.load_subscribe_facts(sid_a).unwrap();
    assert!(rem_a.get(Some(2), Some(1)).is_none());
    assert_eq!(
        rem_a.get(Some(2), Some(2)).and_then(|q| q.path.as_deref()),
        Some(f.shared.to_string_lossy().as_ref())
    );
    let rem_b = store.load_subscribe_facts(sid_b).unwrap();
    assert_eq!(rem_b.entries().count(), 2);
}

#[tokio::test]
async fn delete_subscription_preserves_shared_multiseason_file_owned_by_other_subscribe() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let f = setup_shared_multiseason_fixture(&app, &tmp).await;

    let resp = app
        .oneshot(request(
            "DELETE",
            &format!(
                "/api/v1/subscriptions/{}?delete_library_files=true",
                f.sub_a
            ),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["data"]["binned"], 1, "{body}");
    assert!(!f.s2e1.exists(), "exclusively owned s2e1 must be deleted");
    assert!(
        f.shared.exists(),
        "shared multi-season file owned by Subscribe B must survive delete_subscription"
    );
    assert!(f.s1e1.exists());

    let store = Store::open(tmp.path().join("data")).unwrap();
    let remaining = store
        .file_coverage_for_media(f.media_id.parse().unwrap())
        .unwrap();
    assert_eq!(remaining.len(), 2);
}
