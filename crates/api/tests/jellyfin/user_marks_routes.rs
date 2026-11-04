use super::*;

#[tokio::test]
async fn vidhub_post_delete_clears_favorite_and_played_marks_after_reentry() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let (app, media_id, item_id) = app(tmp.path());
    let user_id = domain::UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();

    for prefix in ["", "/emby"] {
        for (mark, field) in [("FavoriteItems", "IsFavorite"), ("PlayedItems", "Played")] {
            let path = format!("{prefix}/Users/{user_id}/{mark}/{item_id}");
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(&path)
                        .header("authorization", "Bearer admin-token")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "set {path}");
            assert_eq!(json(response).await[field], true);

            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("{path}/Delete"))
                        .header("authorization", "Bearer admin-token")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "clear {path}");
            assert_eq!(json(response).await[field], false);
            let details = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/Users/{user_id}/Items/{item_id}"))
                        .header("authorization", "Bearer admin-token")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(details.status(), StatusCode::OK);
            let details = json(details).await;
            assert_eq!(details["UserData"][field], false, "reentry {path}");
            let marks = Store::open(tmp.path().join("data"))
                .unwrap()
                .playback_marks(user_id, media_id)
                .unwrap()
                .unwrap();
            assert!(!if field == "IsFavorite" {
                marks.1
            } else {
                marks.0
            });
        }
    }

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/Users/{user_id}/FavoriteItems/{item_id}/Delete"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn favorite_and_played_routes_update_the_authenticated_users_marks() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media_id, item_id) = app(tmp.path());
    let user_id = domain::UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();

    for (method, uri, favorite, played) in [
        (
            "POST",
            format!("/Users/{user_id}/FavoriteItems/{item_id}"),
            Some(true),
            None,
        ),
        (
            "DELETE",
            format!("/UserFavoriteItems/{item_id}"),
            Some(false),
            None,
        ),
        (
            "POST",
            format!("/UserPlayedItems/{item_id}"),
            None,
            Some(true),
        ),
        (
            "DELETE",
            format!("/Users/{user_id}/PlayedItems/{item_id}"),
            None,
            Some(false),
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(&uri)
                    .header("authorization", "Bearer admin-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{method} {uri}");
        let marks = Store::open(tmp.path().join("data"))
            .unwrap()
            .playback_marks(user_id, media_id)
            .unwrap()
            .unwrap();
        if let Some(expected) = favorite {
            assert_eq!(marks.1, expected, "{method} {uri}");
        }
        if let Some(expected) = played {
            assert_eq!(marks.0, expected, "{method} {uri}");
        }
    }
}
