//! Title preview and creating a Subscription from a title reference (TMDB and douban).

use super::support::*;

#[tokio::test]
async fn title_preview_resolves_tmdb_ref_to_ready() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _downloader) = fake_state(&tmp);
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions/title-preview",
            Some("management-secret"),
            json!({ "title_ref": "tmdb:movie:603" }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["data"]["status"], "ready");
    assert_eq!(body["data"]["media"]["title"], "The Matrix");
    assert_eq!(body["data"]["media"]["tmdb_id"], "603");
    assert_eq!(body["data"]["movie_owned"], false);
    assert_eq!(body["data"]["existing_subscription_id"], Value::Null);
}

#[tokio::test]
async fn title_preview_bare_title_returns_ambiguous_candidates() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _downloader) = fake_state(&tmp);
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions/title-preview",
            Some("management-secret"),
            json!({ "title_ref": "matrix" }),
        ))
        .await
        .unwrap();
    let body = json_body(response).await;
    assert_eq!(body["data"]["status"], "ambiguous");
    let candidates = body["data"]["candidates"].as_array().unwrap();
    assert!(
        candidates.len() >= 2,
        "裸标题多命中应给候选墙: {candidates:?}"
    );
    let refs: Vec<&str> = candidates
        .iter()
        .filter_map(|c| c["title_ref"].as_str())
        .collect();
    assert!(refs.contains(&"tmdb:movie:603"), "候选应含电影: {refs:?}");
    assert!(refs.contains(&"tmdb:tv:624860"), "候选应含剧集: {refs:?}");
}

#[tokio::test]
async fn title_preview_unknown_ref_returns_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _downloader) = fake_state(&tmp);
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions/title-preview",
            Some("management-secret"),
            json!({ "title_ref": "tmdb:movie:999999" }),
        ))
        .await
        .unwrap();
    let body = json_body(response).await;
    assert_eq!(body["data"]["status"], "not_found");
}

#[tokio::test]
async fn create_subscription_accepts_title_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _downloader) = fake_state(&tmp);
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({ "title_ref": "tmdb:movie:603", "coverage": { "kind": "movie" } }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = json_body(response).await;
    assert_eq!(body["data"]["media"]["title"], "The Matrix");
    assert_eq!(body["data"]["media"]["kind"], "movie");
}

#[tokio::test]
async fn title_preview_resolves_douban_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let app = douban_app(&tmp);
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions/title-preview",
            Some("management-secret"),
            json!({ "title_ref": "douban:movie:1292052" }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["data"]["status"], "ready", "douban:id 应解析成功");
    assert_eq!(body["data"]["media"]["title"], "肖申克的救赎");
    assert_eq!(body["data"]["media"]["douban_id"], "1292052");
    assert_eq!(body["data"]["media"]["kind"], "movie");
}

#[tokio::test]
async fn create_subscription_accepts_douban_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let app = douban_app(&tmp);
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({
                "title_ref": "douban:movie:1292052",
                "coverage": { "kind": "movie" },
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = json_body(response).await;
    assert_eq!(body["data"]["media"]["title"], "肖申克的救赎");
    assert_eq!(body["data"]["media"]["douban_id"], "1292052");
}

#[tokio::test]
async fn title_preview_does_not_leak_other_user_subscription_or_block_different_season() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _downloader) = fake_state(&tmp);
    let (_alice_id, alice_token) = create_member(&app, "alice").await;
    let (_bob_id, bob_token) = create_member(&app, "bob").await;

    // Alice 订阅了电影 The Matrix (tmdb:movie:603)
    let alice_sub = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some(&alice_token),
            json!({
                "title_ref": "tmdb:movie:603",
                "coverage": { "kind": "movie" },
            }),
        ))
        .await
        .unwrap();
    assert_eq!(alice_sub.status(), StatusCode::CREATED);

    // Bob 打开同部电影的 title-preview，返回的 existing_subscription_id 必须是 null！绝不能泄漏或被 Alice 阻挡
    let bob_preview = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions/title-preview",
            Some(&bob_token),
            json!({
                "title_ref": "tmdb:movie:603",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(bob_preview.status(), StatusCode::OK);
    let body = json_body(bob_preview).await;
    assert_eq!(
        body["data"]["existing_subscription_id"],
        Value::Null,
        "用户 B 的预检结果绝不可被用户 A 已有订阅占领"
    );
}

