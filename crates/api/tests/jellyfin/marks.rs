use super::*;

#[tokio::test]
async fn vidhub_series_marks_are_returned_after_reentry_with_episode_states_present() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("tv");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Shows",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let show = new_media(MediaKind::Tv, "Show");
    store.insert_media(&show).unwrap();
    insert_episode(&store, &root, show.id, 1, 1);
    insert_episode(&store, &root, show.id, 1, 2);
    let user_id = domain::UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    for episode in [1, 2] {
        store
            .upsert_unit(
                user_id,
                show.id,
                1,
                episode,
                0,
                Some(false),
                Some(false),
                None,
                None,
                None,
                false,
                1,
            )
            .unwrap();
    }
    let series_id = show.id.to_string().replace('-', "");

    for (mark, field) in [("FavoriteItems", "IsFavorite"), ("PlayedItems", "Played")] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/Users/admin/{mark}/{series_id}"))
                    .header("authorization", "Bearer admin-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{mark}");
        let data = json(response).await;
        assert_eq!(data[field], true, "{mark}");
        assert_eq!(data["Key"], series_id, "{mark}");
        assert_eq!(data["ItemId"], series_id, "{mark}");
        assert_eq!(
            data["UnplayedItemCount"],
            if field == "Played" { 0 } else { 2 }
        );
    }

    let stored = Store::open(tmp.path().join("data"))
        .unwrap()
        .unit_state(
            user_id,
            show.id,
            api::store::UNIT_WHOLE,
            api::store::UNIT_WHOLE,
        )
        .unwrap()
        .unwrap();
    assert!(stored.favorite);
    assert!(stored.played);

    let reopened_details = get_json(&app, &format!("/Users/admin/Items/{series_id}")).await;
    assert_eq!(reopened_details["UserData"]["IsFavorite"], true);
    assert_eq!(
        reopened_details["UserData"]["Played"], true,
        "the series mark must take precedence over older unplayed episode rows"
    );
    for filter in ["IsFavorite", "IsPlayed"] {
        let listed = get_json(&app, &format!("/Users/admin/Items?Filters={filter}")).await;
        assert_eq!(listed["TotalRecordCount"], 1, "Filters={filter}");
        assert_eq!(listed["Items"][0]["Name"], "Show", "Filters={filter}");
    }

    for (mark, field) in [("FavoriteItems", "IsFavorite"), ("PlayedItems", "Played")] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/Users/admin/{mark}/{series_id}"))
                    .header("authorization", "Bearer admin-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{mark}");
        let data = json(response).await;
        assert_eq!(data[field], false, "{mark}");
        assert_eq!(data["Key"], series_id, "{mark}");
        assert_eq!(data["ItemId"], series_id, "{mark}");
    }

    let cleared_details = get_json(&app, &format!("/Users/admin/Items/{series_id}")).await;
    assert_eq!(cleared_details["UserData"]["IsFavorite"], false);
    assert_eq!(cleared_details["UserData"]["Played"], false);
    for filter in ["IsFavorite", "IsPlayed"] {
        let listed = get_json(&app, &format!("/Users/admin/Items?Filters={filter}")).await;
        assert_eq!(listed["TotalRecordCount"], 0, "Filters={filter}");
    }
}
