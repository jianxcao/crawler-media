use axum::http::StatusCode;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::super::common::*;
use super::{create_named_rule_set, locking_preview_app, locking_preview_state};

struct PrivateRoutingFixture {
    default_lib_id: String,
    private_lib_id: String,
    default_filter: String,
    private_filter: String,
    outsider_token: String,
    allowed_token: String,
}

async fn setup_private_routing(
    app: &axum::Router,
    tmp: &tempfile::TempDir,
) -> PrivateRoutingFixture {
    let (_outsider_id, outsider_token) = create_member(app, "outsider-member").await;
    let (allowed_id, allowed_token) = create_member(app, "allowed-member").await;
    let default_filter = create_named_rule_set(app, "tv-public-filter").await;
    let private_filter = create_named_rule_set(app, "tv-private-cn-filter").await;

    let public_root = tmp.path().join("tv-public");
    let private_root = tmp.path().join("tv-private-secret-cn");
    std::fs::create_dir_all(&public_root).unwrap();
    std::fs::create_dir_all(&private_root).unwrap();

    let default_lib = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                json!({
                    "name": "公共剧集库",
                    "kind": "tv",
                    "root_paths": [public_root.display().to_string()],
                    "default_filter_id": default_filter,
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let default_lib_id = default_lib["id"].as_str().unwrap().to_string();
    let set_default = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/api/v1/libraries/{default_lib_id}/default"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(set_default.status(), StatusCode::OK);

    let private_lib = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                json!({
                    "name": "私密国产剧库",
                    "kind": "tv",
                    "root_paths": [private_root.display().to_string()],
                    "access_mode": "selected",
                    "member_ids": [allowed_id.to_string()],
                    "default_filter_id": private_filter,
                    "match_rules": [{
                        "field": "origin_countries",
                        "op": "any_of",
                        "values": ["CN"]
                    }],
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    PrivateRoutingFixture {
        default_lib_id,
        private_lib_id: private_lib["id"].as_str().unwrap().to_string(),
        default_filter,
        private_filter,
        outsider_token,
        allowed_token,
    }
}

#[tokio::test]
async fn outsider_member_cannot_discover_selected_library_in_routing_preview() {
    let tmp = tempfile::tempdir().unwrap();
    let app = locking_preview_app(&tmp);
    let fx = setup_private_routing(&app, &tmp).await;

    let auto_preview = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions/download-routing-preview",
                Some(&fx.outsider_token),
                json!({ "kind": "tv", "tmdb_id": "203483", "title": "两京十五日" }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(auto_preview["library_id"], fx.default_lib_id);
    assert!(auto_preview["route_matched"].is_null());
    assert!(!auto_preview.to_string().contains("私密国产剧库"));
    assert!(!auto_preview.to_string().contains("tv-private-secret-cn"));

    let explicit_preview = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions/download-routing-preview",
                Some(&fx.outsider_token),
                json!({
                    "kind": "tv",
                    "library_id": fx.private_lib_id,
                    "title": "两京十五日"
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(explicit_preview["ok"], false);
    assert!(explicit_preview["library_id"].is_null());
    assert!(explicit_preview["library_name"].is_null());
    assert!(explicit_preview["entry_dir"].is_null());
    assert_eq!(explicit_preview["warning"], "指定的媒体库不存在");
    assert!(!explicit_preview.to_string().contains("私密国产剧库"));
    assert!(
        !explicit_preview
            .to_string()
            .contains("tv-private-secret-cn")
    );
}

#[tokio::test]
async fn outsider_member_cannot_auto_route_or_target_selected_library_in_post_or_patch() {
    let tmp = tempfile::tempdir().unwrap();
    let app = locking_preview_app(&tmp);
    let fx = setup_private_routing(&app, &tmp).await;

    let explicit_post = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some(&fx.outsider_token),
            json!({
                "title_ref": "tmdb:tv:203483",
                "coverage": { "kind": "tv", "season": 1, "episode_from": 1 },
                "fetch_mode": "search",
                "library_id": fx.private_lib_id
            }),
        ))
        .await
        .unwrap();
    assert_eq!(explicit_post.status(), StatusCode::BAD_REQUEST);
    let post_err = json_error(explicit_post).await;
    assert_eq!(post_err["code"], "subscription.invalid");
    assert_eq!(post_err["message"], "指定的媒体库不存在");

    let auto_post = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some(&fx.outsider_token),
            json!({
                "title_ref": "tmdb:tv:203483",
                "coverage": { "kind": "tv", "season": 1, "episode_from": 1 },
                "fetch_mode": "search"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(auto_post.status(), StatusCode::CREATED);
    let sub = json_data(auto_post).await;
    assert!(sub["library_id"].is_null());
    assert_eq!(sub["filter_id"], fx.default_filter);

    let sub_id = sub["id"].as_str().unwrap();
    let patch_res = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/subscriptions/{sub_id}"),
            Some(&fx.outsider_token),
            json!({ "library_id": fx.private_lib_id }),
        ))
        .await
        .unwrap();
    assert_eq!(patch_res.status(), StatusCode::BAD_REQUEST);
    let patch_err = json_error(patch_res).await;
    assert_eq!(patch_err["code"], "subscription.invalid");
    assert_eq!(patch_err["message"], "指定的媒体库不存在");
}

#[tokio::test]
async fn authorized_selected_member_can_auto_route_and_explicitly_target_library() {
    let tmp = tempfile::tempdir().unwrap();
    let app = locking_preview_app(&tmp);
    let fx = setup_private_routing(&app, &tmp).await;

    let preview = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions/download-routing-preview",
                Some(&fx.allowed_token),
                json!({ "kind": "tv", "tmdb_id": "203483", "title": "两京十五日" }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(preview["library_id"], fx.private_lib_id);
    assert_eq!(preview["route_matched"], true);

    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some(&fx.allowed_token),
            json!({
                "title_ref": "tmdb:tv:203483",
                "coverage": { "kind": "tv", "season": 1, "episode_from": 1 },
                "fetch_mode": "search"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let sub = json_data(created).await;
    assert_eq!(sub["library_id"], fx.private_lib_id);
    assert_eq!(sub["filter_id"], fx.private_filter);

    let sub_id = sub["id"].as_str().unwrap();
    let to_default = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/subscriptions/{sub_id}"),
            Some(&fx.allowed_token),
            json!({ "library_id": fx.default_lib_id }),
        ))
        .await
        .unwrap();
    assert_eq!(to_default.status(), StatusCode::OK);

    let back_to_private = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/subscriptions/{sub_id}"),
            Some(&fx.allowed_token),
            json!({ "library_id": fx.private_lib_id }),
        ))
        .await
        .unwrap();
    assert_eq!(back_to_private.status(), StatusCode::OK);
    assert_eq!(
        json_data(back_to_private).await["library_id"],
        fx.private_lib_id
    );
}

#[tokio::test]
async fn routing_preview_validates_explicit_library_and_skips_unmatched_rule_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let state = locking_preview_state(&tmp);
    let store = state.store();
    let app = api::router(state);

    let tv_lib = store
        .default_library(domain::MediaKind::Tv)
        .unwrap()
        .unwrap();
    let mismatch = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions/download-routing-preview",
                Some("management-secret"),
                json!({ "kind": "movie", "library_id": tv_lib.id, "title": "Inception" }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(mismatch["ok"], false);
    assert_eq!(mismatch["warning"], "订阅媒体类型与目标库类型不匹配");

    let anime_root = tmp.path().join("movie-anime-only");
    std::fs::create_dir_all(&anime_root).unwrap();
    let anime = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                json!({
                    "name": "动画电影专库",
                    "kind": "movie",
                    "root_paths": [anime_root.display().to_string()],
                    "match_rules": [{ "field": "genres", "op": "any_of", "values": [16] }],
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let anime_id = anime["id"].as_str().unwrap().to_string();

    let default_movie = store
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .unwrap();
    store.begin_library_removal(&default_movie.id).unwrap();

    let removing_explicit = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions/download-routing-preview",
                Some("management-secret"),
                json!({ "kind": "movie", "library_id": default_movie.id, "title": "Matrix" }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(removing_explicit["ok"], false);
    assert_eq!(removing_explicit["warning"], "目标媒体库正在删除或重建");

    let unmatched = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions/download-routing-preview",
                Some("management-secret"),
                json!({ "kind": "movie", "title": "Non-Anime Drama" }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        unmatched["ok"], false,
        "must not fall back to non-default specialized match_rules library {anime_id}: {unmatched}"
    );
    assert!(unmatched["library_id"].is_null());
}

async fn setup_restricted_default_tv(
    app: &axum::Router,
    store: &api::Store,
    tmp: &tempfile::TempDir,
) -> (String, String, String, String) {
    let preseeded_id = store
        .default_library(domain::MediaKind::Tv)
        .unwrap()
        .map(|lib| lib.id);
    let (_outsider_id, outsider_token) = create_member(app, "outsider-def").await;
    let (allowed_id, allowed_token) = create_member(app, "allowed-def").await;
    let restricted_filter = create_named_rule_set(app, "restricted-def-filter").await;
    let restricted_root = tmp.path().join("tv-restricted-default");
    std::fs::create_dir_all(&restricted_root).unwrap();
    let restricted_lib = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                json!({
                    "name": "受限默认剧库",
                    "kind": "tv",
                    "root_paths": [restricted_root.display().to_string()],
                    "access_mode": "selected",
                    "member_ids": [allowed_id.to_string()],
                    "default_filter_id": restricted_filter,
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let restricted_lib_id = restricted_lib["id"].as_str().unwrap().to_string();
    assert_eq!(
        app.clone()
            .oneshot(request(
                "PUT",
                &format!("/api/v1/libraries/{restricted_lib_id}/default"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    if let Some(preseeded_id) = preseeded_id {
        store.begin_library_removal(&preseeded_id).unwrap();
    }
    (
        restricted_lib_id,
        restricted_filter,
        outsider_token,
        allowed_token,
    )
}

async fn post_tv_lib(app: &axum::Router, root: &std::path::Path, mut body: Value) -> String {
    body["kind"] = json!("tv");
    body["root_paths"] = json!([root.display().to_string()]);
    let created = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                body,
            ))
            .await
            .unwrap(),
    )
    .await;
    created["id"].as_str().unwrap().to_string()
}

async fn set_default_lib(app: &axum::Router, lib_id: &str) {
    let resp = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/api/v1/libraries/{lib_id}/default"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn restricted_default_library_rejects_outsider_until_fallback_exists_and_blocks_null_patch() {
    let tmp = tempfile::tempdir().unwrap();
    let state = locking_preview_state(&tmp);
    let store = state.store();
    let app = api::router(state);
    let (_restricted_id, restricted_filter, outsider_token, allowed_token) =
        setup_restricted_default_tv(&app, &store, &tmp).await;

    let rejected = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some(&outsider_token),
            json!({
                "title_ref": "tmdb:tv:1399",
                "coverage": { "kind": "tv", "season": 1, "episode_from": 1 },
                "fetch_mode": "search"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    let rej_err = json_error(rejected).await;
    assert_eq!(rej_err["code"], "subscription.invalid");
    assert_eq!(rej_err["message"], "没有可用的目标媒体库");

    let fallback_filter = create_named_rule_set(&app, "public-fallback-filter").await;
    let public_root = tmp.path().join("tv-public-secondary");
    std::fs::create_dir_all(&public_root).unwrap();
    let public_lib_id = post_tv_lib(
        &app,
        &public_root,
        json!({ "name": "公开备用剧库", "default_filter_id": fallback_filter }),
    )
    .await;

    let sub = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some(&outsider_token),
                json!({
                    "title_ref": "tmdb:tv:1399",
                    "coverage": { "kind": "tv", "season": 1, "episode_from": 1 },
                    "fetch_mode": "search"
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(sub["library_id"], public_lib_id);
    assert_eq!(sub["filter_id"], fallback_filter);

    let sub_id = sub["id"].as_str().unwrap();
    for raw_lib in [Value::Null, json!("")] {
        let patch_null = app
            .clone()
            .oneshot(request(
                "PATCH",
                &format!("/api/v1/subscriptions/{sub_id}"),
                Some(&outsider_token),
                json!({ "library_id": raw_lib }),
            ))
            .await
            .unwrap();
        assert_eq!(patch_null.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            json_error(patch_null).await["message"],
            "指定的媒体库不存在"
        );
    }

    let allowed_sub = json_data(
        app.oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some(&allowed_token),
            json!({
                "title_ref": "tmdb:tv:1399",
                "coverage": { "kind": "tv", "season": 1, "episode_from": 1 },
                "fetch_mode": "search"
            }),
        ))
        .await
        .unwrap(),
    )
    .await;
    assert!(allowed_sub["library_id"].is_null());
    assert_eq!(allowed_sub["filter_id"], restricted_filter);
}

#[tokio::test]
async fn admin_auto_routing_skips_hidden_library_while_allowing_explicit_target() {
    let tmp = tempfile::tempdir().unwrap();
    let app = locking_preview_app(&tmp);
    let public_filter = create_named_rule_set(&app, "visible-tv-filter").await;
    let hidden_filter = create_named_rule_set(&app, "hidden-cn-filter").await;
    let visible_root = tmp.path().join("tv-visible");
    let hidden_root = tmp.path().join("tv-hidden-cn");
    std::fs::create_dir_all(&visible_root).unwrap();
    std::fs::create_dir_all(&hidden_root).unwrap();

    let visible_lib_id = post_tv_lib(
        &app,
        &visible_root,
        json!({ "name": "可见默认剧库", "default_filter_id": public_filter }),
    )
    .await;
    set_default_lib(&app, &visible_lib_id).await;

    let hidden_lib_id = post_tv_lib(
        &app,
        &hidden_root,
        json!({
            "name": "管理员隐藏国产剧库",
            "admin_visible": false,
            "default_filter_id": hidden_filter,
            "match_rules": [{ "field": "origin_countries", "op": "any_of", "values": ["CN"] }],
        }),
    )
    .await;

    let preview = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions/download-routing-preview",
                Some("management-secret"),
                json!({ "kind": "tv", "tmdb_id": "203483", "title": "两京十五日" }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(preview["library_id"], visible_lib_id);
    assert!(preview["route_matched"].is_null());

    let admin_auto = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "title_ref": "tmdb:tv:203483",
                    "coverage": { "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2 },
                    "fetch_mode": "search"
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(admin_auto["library_id"].is_null());
    assert_eq!(admin_auto["filter_id"], public_filter);

    let admin_explicit = app
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({
                "title_ref": "tmdb:tv:203483",
                "coverage": { "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2 },
                "fetch_mode": "search",
                "library_id": hidden_lib_id
            }),
        ))
        .await
        .unwrap();
    assert_eq!(admin_explicit.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn overlap_conflict_redacts_other_member_subscription_uuid() {
    let tmp = tempfile::tempdir().unwrap();
    let app = locking_preview_app(&tmp);
    let (_, member_a_token) = create_member(&app, "overlap-a").await;
    let (_, member_b_token) = create_member(&app, "overlap-b").await;
    let fx = setup_private_routing(&app, &tmp).await;

    let a_sub = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some(&member_a_token),
                json!({
                    "title_ref": "tmdb:tv:1399",
                    "coverage": { "kind": "tv", "season": 2, "episode_from": 1, "episode_to": 3 },
                    "fetch_mode": "search",
                    "library_id": fx.default_lib_id
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let a_sub_id = a_sub["id"].as_str().unwrap();

    let b_conflict = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some(&member_b_token),
            json!({
                "title_ref": "tmdb:tv:1399",
                "coverage": { "kind": "tv", "season": 2, "episode_from": 2, "episode_to": 4 },
                "fetch_mode": "search",
                "library_id": fx.default_lib_id
            }),
        ))
        .await
        .unwrap();
    assert_eq!(b_conflict.status(), StatusCode::CONFLICT);
    let b_err = json_error(b_conflict).await;
    assert_eq!(b_err["code"], "subscription.overlap");
    assert_eq!(
        b_err["message"],
        "同一媒体库中已有覆盖重叠的订阅，请调整集数范围"
    );
    assert!(!b_err["message"].as_str().unwrap().contains(a_sub_id));
}

#[tokio::test]
async fn patch_null_library_id_rejects_hidden_or_deleting_default_library() {
    let tmp = tempfile::tempdir().unwrap();
    let state = locking_preview_state(&tmp);
    let store = state.store();
    let app = api::router(state);

    let secondary_root = tmp.path().join("tv-secondary-visible");
    let hidden_def_root = tmp.path().join("tv-hidden-default");
    std::fs::create_dir_all(&secondary_root).unwrap();
    std::fs::create_dir_all(&hidden_def_root).unwrap();

    let secondary_id = post_tv_lib(&app, &secondary_root, json!({ "name": "可见备用剧库" })).await;
    let hidden_def_id = post_tv_lib(
        &app,
        &hidden_def_root,
        json!({ "name": "管理员隐藏默认剧库", "admin_visible": false }),
    )
    .await;
    set_default_lib(&app, &hidden_def_id).await;

    let sub = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "title_ref": "tmdb:tv:1399",
                    "coverage": { "kind": "tv", "season": 1, "episode_from": 1 },
                    "fetch_mode": "search",
                    "library_id": secondary_id
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let sub_id = sub["id"].as_str().unwrap();

    for raw_lib in [Value::Null, json!("")] {
        let res = app
            .clone()
            .oneshot(request(
                "PATCH",
                &format!("/api/v1/subscriptions/{sub_id}"),
                Some("management-secret"),
                json!({ "library_id": raw_lib }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let err = json_error(res).await;
        assert_eq!(err["code"], "subscription.invalid");
        assert_eq!(err["message"], "指定的媒体库不存在");
    }

    store
        .with_writer(store::Db::Library, |conn| {
            conn.execute("UPDATE libraries SET is_default = 0 WHERE kind = 'tv'", [])?;
            Ok(())
        })
        .unwrap();
    for raw_lib in [Value::Null, json!("")] {
        let no_def = app
            .clone()
            .oneshot(request(
                "PATCH",
                &format!("/api/v1/subscriptions/{sub_id}"),
                Some("management-secret"),
                json!({ "library_id": raw_lib }),
            ))
            .await
            .unwrap();
        assert_eq!(no_def.status(), StatusCode::BAD_REQUEST);
        let err = json_error(no_def).await;
        assert_eq!(err["code"], "subscription.invalid");
        assert_eq!(err["message"], "没有可用的目标媒体库");
    }

    set_default_lib(&app, &secondary_id).await;
    store.begin_library_removal(&secondary_id).unwrap();

    let conflict = app
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/subscriptions/{sub_id}"),
            Some("management-secret"),
            json!({ "library_id": Value::Null }),
        ))
        .await
        .unwrap();
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    let conflict_err = json_error(conflict).await;
    assert_eq!(conflict_err["code"], "subscription.library_unavailable");
    assert_eq!(conflict_err["message"], "目标媒体库正在删除或重建");
}
