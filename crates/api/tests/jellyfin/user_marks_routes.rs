use super::*;

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
            format!("/UserPlayedItems/{media_id}"),
            None,
            Some(true),
        ),
        (
            "DELETE",
            format!("/Users/{user_id}/PlayedItems/{media_id}"),
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
