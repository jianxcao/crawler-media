//! Movie coordinates used by the actual web player must preserve historical watch state.
use axum::http::StatusCode;
use serde_json::json;

use super::common::{json_data, request};
use super::playback_review::{fixture, post, save_position};
use tower::ServiceExt;

#[tokio::test]
async fn web_movie_zero_unit_keeps_resume_and_completed_marks() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, mut target) = fixture(&tmp, false);
    target["season_number"] = json!(0);
    target["episode_number"] = json!(0);
    save_position(&app, &target).await;
    assert_eq!(
        post(&app, "decide", target.clone()).await.status(),
        StatusCode::OK
    );
    let session = json_data(post(&app, "sessions", target.clone()).await).await;
    assert_eq!(session["start_ms"], 600_000);
    let mut mark = target.clone();
    mark["played"] = json!(true);
    assert_eq!(post(&app, "marks", mark).await.status(), StatusCode::OK);
    let session = json_data(post(&app, "sessions", target).await).await;
    assert_eq!(session["start_ms"], 0);
}

#[tokio::test]
async fn frontend_resume_query_reads_saved_movie_progress() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, mut target) = fixture(&tmp, false);
    target["season_number"] = json!(0);
    target["episode_number"] = json!(0);
    save_position(&app, &target).await;
    let uri = format!(
        "/api/v1/playback/resume?media_item_id={}&season_number=0&episode_number=0",
        target["media_item_id"].as_str().unwrap()
    );
    let response = app
        .oneshot(request(
            "GET",
            &uri,
            Some("management-secret"),
            serde_json::Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(json_data(response).await["position_ms"], 600_000);
}

#[tokio::test]
async fn frontend_movie_marks_query_reads_saved_progress_as_played_state() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, mut target) = fixture(&tmp, false);
    target["season_number"] = json!(0);
    target["episode_number"] = json!(0);
    save_position(&app, &target).await;
    let mut mark = target.clone();
    mark["played"] = json!(true);
    assert_eq!(post(&app, "marks", mark).await.status(), StatusCode::OK);
    let uri = format!(
        "/api/v1/playback/marks?media_item_id={}&season_number=0&episode_number=0",
        target["media_item_id"].as_str().unwrap()
    );
    let response = app
        .clone()
        .oneshot(request(
            "GET",
            &uri,
            Some("management-secret"),
            serde_json::Value::Null,
        ))
        .await
        .unwrap();
    let marks = json_data(response).await;
    assert_eq!(marks["played"], true);
    let resume = json_data(
        app.oneshot(request(
            "GET",
            &format!(
                "/api/v1/playback/resume?media_item_id={}&season_number=0&episode_number=0",
                target["media_item_id"].as_str().unwrap()
            ),
            Some("management-secret"),
            serde_json::Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(resume["played"], true);
}

#[tokio::test]
async fn tv_special_zero_unit_is_not_aliased_to_whole_show() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, mut target) = fixture(&tmp, true);
    target.as_object_mut().unwrap().remove("file_id");
    target["season_number"] = json!(0);
    target["episode_number"] = json!(0);
    target["device_id"] = json!("special-device");
    target["position_ms"] = json!(600_000);
    assert_eq!(
        post(&app, "progress", target).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn canonical_zero_progress_overrides_historical_progress() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, mut target) = fixture(&tmp, false);
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let media: domain::MediaId = target["media_item_id"].as_str().unwrap().parse().unwrap();
    let user = store
        .list_users()
        .unwrap()
        .into_iter()
        .find(|u| u.role == domain::UserRole::Admin)
        .unwrap()
        .id;
    store
        .upsert_unit(
            user, media, 0, 0, 600_000, None, None, None, None, None, false, 1,
        )
        .unwrap();
    target["season_number"] = json!(0);
    target["episode_number"] = json!(0);
    target["device_id"] = json!("replay-device");
    target["position_ms"] = json!(0);
    assert_eq!(
        post(&app, "progress", target.clone()).await.status(),
        StatusCode::OK
    );
    let uri = format!(
        "/api/v1/playback/resume?media_item_id={}&season_number=0&episode_number=0",
        target["media_item_id"].as_str().unwrap()
    );
    let response = app
        .oneshot(request(
            "GET",
            &uri,
            Some("management-secret"),
            serde_json::Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(json_data(response).await["position_ms"], 0);
}

#[tokio::test]
async fn whole_tv_marks_aggregate_child_favorites_despite_explicit_false() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, target) = fixture(&tmp, true);
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let media: domain::MediaId = target["media_item_id"].as_str().unwrap().parse().unwrap();
    let user = store
        .list_users()
        .unwrap()
        .into_iter()
        .find(|u| u.role == domain::UserRole::Admin)
        .unwrap()
        .id;
    store
        .upsert_unit(
            user,
            media,
            -1,
            -1,
            0,
            None,
            Some(false),
            None,
            None,
            None,
            false,
            1,
        )
        .unwrap();
    store
        .upsert_unit(
            user,
            media,
            2,
            3,
            0,
            None,
            Some(true),
            None,
            None,
            None,
            false,
            2,
        )
        .unwrap();
    let uri = format!("/api/v1/playback/marks?media_item_id={}", media);
    let response = app
        .oneshot(request(
            "GET",
            &uri,
            Some("management-secret"),
            serde_json::Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(json_data(response).await["is_favorite"], true);
}

#[tokio::test]
async fn tv_whole_resume_must_not_read_special_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, target) = fixture(&tmp, true);
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let media: domain::MediaId = target["media_item_id"].as_str().unwrap().parse().unwrap();
    let user = store
        .list_users()
        .unwrap()
        .into_iter()
        .find(|u| u.role == domain::UserRole::Admin)
        .unwrap()
        .id;
    store
        .upsert_unit(
            user, media, 0, 0, 600_000, None, None, None, None, None, false, 1,
        )
        .unwrap();
    let uri = format!("/api/v1/playback/resume?media_item_id={}", media);
    let response = app
        .oneshot(request(
            "GET",
            &uri,
            Some("management-secret"),
            serde_json::Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(json_data(response).await["position_ms"], 0);
}

#[tokio::test]
async fn historical_movie_metadata_survives_first_canonical_progress() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, mut target) = fixture(&tmp, false);
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let media: domain::MediaId = target["media_item_id"].as_str().unwrap().parse().unwrap();
    let user = store
        .list_users()
        .unwrap()
        .into_iter()
        .find(|u| u.role == domain::UserRole::Admin)
        .unwrap()
        .id;
    store
        .upsert_unit(
            user,
            media,
            0,
            0,
            600_000,
            None,
            Some(true),
            Some(7_200_000),
            Some("audio-old"),
            Some("subtitle-old"),
            true,
            1,
        )
        .unwrap();
    target["season_number"] = json!(0);
    target["episode_number"] = json!(0);
    save_position(&app, &target).await;
    let uri = format!(
        "/api/v1/playback/resume?media_item_id={}&season_number=0&episode_number=0",
        target["media_item_id"].as_str().unwrap()
    );
    let resume = json_data(
        app.oneshot(request(
            "GET",
            &uri,
            Some("management-secret"),
            serde_json::Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(resume["position_ms"], 600_000);
    assert_eq!(resume["is_favorite"], true);
    assert_eq!(resume["audio_track"], "audio-old");
    assert_eq!(resume["subtitle_track"], "subtitle-old");
    assert_eq!(resume["play_count"], 1);
    assert_eq!(resume["duration_ms"], 7_200_000);
}

#[tokio::test]
async fn historical_state_survives_first_favorite_mark_write() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, mut target) = fixture(&tmp, false);
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let media: domain::MediaId = target["media_item_id"].as_str().unwrap().parse().unwrap();
    let user = store
        .list_users()
        .unwrap()
        .into_iter()
        .find(|u| u.role == domain::UserRole::Admin)
        .unwrap()
        .id;
    store
        .upsert_unit(
            user,
            media,
            0,
            0,
            600_000,
            None,
            Some(true),
            Some(7_200_000),
            Some("audio-old"),
            Some("subtitle-old"),
            true,
            1,
        )
        .unwrap();
    target["season_number"] = json!(0);
    target["episode_number"] = json!(0);
    target["favorite"] = json!(false);
    assert_eq!(
        post(&app, "marks", target.clone()).await.status(),
        StatusCode::OK
    );
    let uri = format!(
        "/api/v1/playback/resume?media_item_id={}&season_number=0&episode_number=0",
        target["media_item_id"].as_str().unwrap()
    );
    let resume = json_data(
        app.oneshot(request(
            "GET",
            &uri,
            Some("management-secret"),
            serde_json::Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(resume["position_ms"], 600_000);
    assert_eq!(resume["audio_track"], "audio-old");
    assert_eq!(resume["play_count"], 1);
}

#[tokio::test]
async fn tv_whole_mark_must_not_migrate_special_zero_progress() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, mut target) = fixture(&tmp, true);
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let media: domain::MediaId = target["media_item_id"].as_str().unwrap().parse().unwrap();
    let user = store
        .list_users()
        .unwrap()
        .into_iter()
        .find(|u| u.role == domain::UserRole::Admin)
        .unwrap()
        .id;
    store
        .upsert_unit(
            user, media, 0, 0, 600_000, None, None, None, None, None, false, 1,
        )
        .unwrap();
    target["favorite"] = json!(true);
    assert_eq!(post(&app, "marks", target).await.status(), StatusCode::OK);
    let uri = format!("/api/v1/playback/resume?media_item_id={media}");
    let resume = json_data(
        app.oneshot(request(
            "GET",
            &uri,
            Some("management-secret"),
            serde_json::Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(
        resume["position_ms"], 0,
        "a TV special's (0,0) progress must never migrate onto the whole series"
    );
}

#[tokio::test]
async fn movie_nonzero_episode_is_not_an_alias_for_whole_movie() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, mut target) = fixture(&tmp, false);
    target["season_number"] = json!(0);
    target["episode_number"] = json!(1);
    assert_eq!(
        post(&app, "sessions", target).await.status(),
        StatusCode::NOT_FOUND
    );
}
