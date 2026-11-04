use super::*;

struct Client {
    app: axum::Router,
    prefix: &'static str,
    token: String,
    user: String,
}

impl Client {
    async fn request(&self, method: &str, path: &str, body: Value) -> axum::response::Response {
        self.app.clone().oneshot(
            Request::builder()
                .method(method)
                .uri(format!("{}{path}", self.prefix))
                .header("authorization", format!("MediaBrowser Client=\"Contract\", Device=\"Test\", DeviceId=\"contract-device\", Version=\"1\", Token=\"{}\"", self.token))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string())).unwrap(),
        ).await.unwrap()
    }

    async fn get(&self, path: &str) -> Value {
        let response = self.request("GET", path, Value::Null).await;
        assert_eq!(response.status(), StatusCode::OK, "GET {path}");
        json(response).await
    }
}

async fn login(app: &axum::Router, prefix: &'static str, name: &str, password: &str) -> Client {
    let response = app.clone().oneshot(
        Request::builder()
            .method("POST")
            .uri(format!("{prefix}/Users/AuthenticateByName"))
            .header("authorization", "MediaBrowser Client=\"Contract\", Device=\"Test\", DeviceId=\"contract-device\", Version=\"1\"")
            .header("content-type", "application/json")
            .body(Body::from(json!({"Username": name, "Pw": password}).to_string())).unwrap(),
    ).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = json(response).await;
    Client {
        app: app.clone(),
        prefix,
        token: response["AccessToken"].as_str().unwrap().to_owned(),
        user: response["User"]["Id"].as_str().unwrap().to_owned(),
    }
}

fn item<'a>(list: &'a Value, id: &str) -> &'a Value {
    list["Items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["Id"] == id)
        .unwrap()
}

async fn assert_positions(
    client: &Client,
    series: &str,
    episode: &str,
    other: &str,
    expected: i64,
) {
    let list = client.get(&format!("/Shows/{series}/Episodes")).await;
    for (id, ticks) in [(episode, expected), (other, 0)] {
        let detail = client
            .get(&format!("/Users/{}/Items/{id}", client.user))
            .await;
        assert_eq!(detail["Id"], id);
        assert_eq!(detail["UserData"]["PlaybackPositionTicks"], ticks);
        assert_eq!(item(&list, id)["UserData"]["PlaybackPositionTicks"], ticks);
    }
}

async fn playback_chain(prefix: &'static str) {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let root = tmp.path().join("tv");
    std::fs::create_dir_all(&root).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
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
    let media = catalog::new_media(MediaKind::Tv, "Contract Show");
    store.insert_media(&media).unwrap();
    let episode = catalog::insert_episode(&store, &root, media.id, 1, 1);
    let episode2 = catalog::insert_episode(&store, &root, media.id, 1, 2);
    let series = work_id(media.id);
    let client = login(&app, prefix, "admin", "admin-token").await;
    let created = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/users")
                .header("authorization", format!("Bearer {}", client.token))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"login":"other", "password":"other-token"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let other = login(&app, prefix, "other", "other-token").await;

    let views = client.get(&format!("/Users/{}/Views", client.user)).await;
    assert!(!views["Items"].as_array().unwrap().is_empty());
    let series_list = client.get("/Items?IncludeItemTypes=Series").await;
    assert_eq!(item(&series_list, &series)["Type"], "Series");
    assert!(item(&series_list, &series).get("MediaSources").is_none());
    assert_positions(&client, &series, &episode, &episode2, 0).await;

    let response = client
        .request("POST", &format!("/Items/{episode}/PlaybackInfo"), json!({}))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let info = json(response).await;
    for path in [
        format!("/Items/{episode}"),
        format!("/Users/{}/Items/{episode}", client.user),
    ] {
        let detail = client.get(&path).await;
        assert_eq!(detail["MediaSources"], info["MediaSources"]);
        assert!(detail.get("PlaySessionId").is_none());
    }
    assert!(
        client
            .get(&format!("/Items/{series}"))
            .await
            .get("MediaSources")
            .is_none()
    );
    let source = info["MediaSources"][0]["Id"].as_str().unwrap();
    let session = info["PlaySessionId"].as_str().unwrap();
    assert!(!source.is_empty() && !session.is_empty());
    let response = app.clone().oneshot(
        Request::builder().uri(format!("{prefix}/Videos/{episode}/stream?MediaSourceId={source}&PlaySessionId={session}"))
            .header("authorization", format!("MediaBrowser Client=\"Contract\", Device=\"Test\", DeviceId=\"contract-device\", Version=\"1\", Token=\"{}\"", client.token))
            .header("range", "bytes=1-3").body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.headers()["content-range"], "bytes 1-3/7");
    assert_eq!(
        &to_bytes(response.into_body(), 1024).await.unwrap()[..],
        b"pis"
    );
    for (path, ticks) in [
        ("/Sessions/Playing", 0),
        ("/Sessions/Playing/Progress", 75_000_000),
        ("/Sessions/Playing/Stopped", 75_000_000),
    ] {
        let response = client.request("POST", path, json!({"ItemId":episode,"MediaSourceId":source,"PlaySessionId":session,"PositionTicks":ticks})).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT, "{path}");
    }
    assert_positions(&client, &series, &episode, &episode2, 75_000_000).await;
    assert_positions(&other, &series, &episode, &episode2, 0).await;
}

#[tokio::test]
async fn authenticated_episode_playback_round_trip_is_scoped_to_user_and_episode() {
    for prefix in ["", "/emby"] {
        playback_chain(prefix).await;
    }
}

async fn movie_chain(prefix: &'static str) {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, movie) = app(tmp.path());
    let client = login(&app, prefix, "admin", "admin-token").await;
    let views = client.get(&format!("/Users/{}/Views", client.user)).await;
    assert!(!views["Items"].as_array().unwrap().is_empty());
    let list = client.get("/Items?IncludeItemTypes=Movie").await;
    assert_eq!(item(&list, &movie)["Type"], "Movie");
    assert!(item(&list, &movie).get("MediaSources").is_none());
    let detail_path = format!("/Users/{}/Items/{movie}", client.user);
    let detail = client.get(&detail_path).await;
    assert_eq!(detail["Id"], movie);
    assert_eq!(detail["UserData"]["PlaybackPositionTicks"], 0);

    let response = client
        .request("POST", &format!("/Items/{movie}/PlaybackInfo"), json!({}))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let info = json(response).await;
    for path in [
        format!("/Items/{movie}"),
        format!("/Users/{}/Items/{movie}", client.user),
    ] {
        let detail = client.get(&path).await;
        assert_eq!(detail["MediaSources"], info["MediaSources"]);
        assert!(detail.get("PlaySessionId").is_none());
    }
    let source = info["MediaSources"][0]["Id"].as_str().unwrap();
    let session = info["PlaySessionId"].as_str().unwrap();
    assert!(!source.is_empty() && !session.is_empty());
    let response = app.clone().oneshot(
        Request::builder().uri(format!("{prefix}/Videos/{movie}/stream?MediaSourceId={source}&PlaySessionId={session}"))
            .header("authorization", format!("MediaBrowser Client=\"Contract\", Device=\"Test\", DeviceId=\"contract-device\", Version=\"1\", Token=\"{}\"", client.token))
            .header("range", "bytes=2-5").body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.headers()["content-range"], "bytes 2-5/16");
    assert_eq!(
        &to_bytes(response.into_body(), 1024).await.unwrap()[..],
        b"2345"
    );
    for (path, ticks) in [
        ("/Sessions/Playing", 0),
        ("/Sessions/Playing/Progress", 50_000_000),
        ("/Sessions/Playing/Stopped", 50_000_000),
    ] {
        let response = client.request("POST", path, json!({"ItemId":movie,"MediaSourceId":source,"PlaySessionId":session,"PositionTicks":ticks})).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT, "{path}");
    }
    let detail = client.get(&detail_path).await;
    let list = client.get("/Items?IncludeItemTypes=Movie").await;
    assert_eq!(detail["UserData"]["PlaybackPositionTicks"], 50_000_000);
    assert_eq!(
        item(&list, &movie)["UserData"]["PlaybackPositionTicks"],
        50_000_000
    );
}

#[tokio::test]
async fn authenticated_movie_playback_round_trip() {
    for prefix in ["", "/emby"] {
        movie_chain(prefix).await;
    }
}
