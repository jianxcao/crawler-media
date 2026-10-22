//! catalog 大类：/Items 的 NFO 元数据、图集与缓存播放流（含公开图片路由）。
//!
//! 原 `tv_items_include_nfo_artwork_metadata_and_cached_playback_streams` 的 243 行
//! 用例按行为阶段拆为 fixture + 命名步骤；断言文本与步骤顺序逐字保留。

use super::*;

/// TV 库 + NFO/poster/fanart/still + 缓存 tracks/chapters/markers 的夹具。
struct TvArtworkFixture {
    _tmp: tempfile::TempDir,
    app: axum::Router,
    _store: Store,
    show_dir: PathBuf,
    series_id: String,
    episode_id: String,
}

/// 铺好 NFO、poster/fanart、剧集 NFO 与缓存播放数据。
async fn tv_artwork_fixture() -> TvArtworkFixture {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("tv");
    let show_dir = root.join("Show");
    let season_dir = show_dir.join("Season 1");
    std::fs::create_dir_all(&season_dir).unwrap();
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
    std::fs::write(
        show_dir.join("tvshow.nfo"),
        "<tvshow><title>Show</title><year>2024</year><plot>Series synopsis</plot><rating>8.7</rating><genre>Drama</genre><actor><name>Series Actor</name><role>Lead</role><tmdbid>101</tmdbid><thumb>/actor.jpg</thumb></actor></tvshow>",
    )
    .unwrap();
    std::fs::write(show_dir.join("poster.jpg"), b"poster").unwrap();
    std::fs::write(show_dir.join("fanart.jpg"), b"fanart").unwrap();
    let episode_path = season_dir.join("Show S01E01.mkv");
    std::fs::write(&episode_path, b"episode").unwrap();
    std::fs::write(
        season_dir.join("Show S01E01.nfo"),
        "<episodedetails><title>Pilot</title><season>1</season><episode>1</episode><plot>Episode synopsis</plot><thumb>https://image.tmdb.org/t/p/w300/still.jpg</thumb></episodedetails>",
    )
    .unwrap();
    let spec = store::test_support::FixtureFile {
        media_id: show.id,
        path: episode_path.display().to_string(),
        season: Some(1),
        episode: Some(1),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
        transfer_mode: None,
        source_path: None,
        file_id: None,
    };
    let published = store::test_support::publish_fixture_file(&store, &spec).unwrap();
    store
        .put_file_meta(
            &published.file_id.to_string(),
            &library::Tracks {
                video: Some(library::VideoTrack {
                    codec: Some("h264".into()),
                    width: Some(1920),
                    height: Some(1080),
                    frame_rate: Some(24.0),
                    bit_rate: Some(4_000_000),
                    duration_secs: Some(600.0),
                    ..Default::default()
                }),
                audio: vec![library::AudioTrack {
                    codec: Some("aac".into()),
                    channels: Some(2),
                    language: Some("ja".into()),
                    sample_rate: Some("48000".into()),
                    is_default: true,
                    ..Default::default()
                }],
                subtitles: vec![library::SubtitleTrack {
                    codec: Some("subrip".into()),
                    language: Some("zh".into()),
                    is_default: true,
                    forced: false,
                    ..Default::default()
                }],
            },
        )
        .unwrap();
    store
        .put_cached_chapters(
            &published.file_id.to_string(),
            &[library::ChapterMarker {
                start_ms: 30_000,
                end_ms: 60_000,
                title: Some("Previously cached chapter".into()),
                marker_type: None,
                synthetic: false,
            }],
        )
        .unwrap();
    store
        .put_media_marker(&api::store::StoredMediaMarker {
            media_id: show.id,
            season: 1,
            episode: 1,
            intro_start_ms: Some(111_000),
            intro_end_ms: Some(222_000),
            outro_start_ms: Some(540_000),
            outro_end_ms: Some(600_000),
            source: "test".into(),
            locked: false,
            updated_at: 1,
        })
        .unwrap();

    let series_id = work_id(show.id);
    let episode_id = unit_id(show.id, 1, 1);
    TvArtworkFixture {
        _tmp: tmp,
        app,
        _store: store,
        show_dir,
        series_id,
        episode_id,
    }
}

/// 剧集（series）明细携带 NFO 元数据与图集标签。
async fn series_detail_carries_nfo_metadata_and_poster_tags(fx: &TvArtworkFixture) {
    let app = fx.app.clone();
    let series_id = fx.series_id.clone();
    let show_dir = &fx.show_dir;
    let series_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/Items/{series_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let series = json(series_response).await;
    assert_eq!(series["Overview"], "Series synopsis");
    assert_eq!(series["CommunityRating"], 8.7);
    assert_eq!(series["Genres"][0], "Drama");
    assert_eq!(series["ProductionYear"], 2024);
    assert_eq!(series["Path"], show_dir.display().to_string());
    assert!(series["MediaStreams"].is_null());
    assert_eq!(series["ChildCount"], 1);
    assert!(
        series["ImageTags"]["Primary"]
            .as_str()
            .is_some_and(|tag| tag.starts_with("poster-"))
    );
    assert_eq!(series["BackdropImageTags"][0], "fanart");
}

/// 收藏/已看标记在 series 明细的 UserData 上往返。
async fn series_marks_round_trip_through_user_data(fx: &TvArtworkFixture) {
    let app = fx.app.clone();
    let series_id = fx.series_id.clone();
    for route in [
        format!("/UserFavoriteItems/{series_id}"),
        format!("/UserPlayedItems/{series_id}"),
    ] {
        let marked = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(route)
                    .header("authorization", "Bearer admin-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(marked.status(), StatusCode::OK);
    }
    let marked_series = get_json(&app, &format!("/Items/{series_id}")).await;
    assert_eq!(marked_series["UserData"]["IsFavorite"], true);
    assert_eq!(marked_series["UserData"]["Played"], true);
}

/// 单集明细携带 NFO 标题、简介与演员。
async fn episode_detail_carries_nfo_title_and_people(fx: &TvArtworkFixture) {
    let app = fx.app.clone();
    let series_id = fx.series_id.clone();
    let episode_id = fx.episode_id.clone();
    let episode_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/Items/{episode_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let episode = json(episode_response).await;
    assert_eq!(episode["Name"], "Pilot");
    assert_eq!(episode["Overview"], "Episode synopsis");
    assert_eq!(episode["IndexNumber"], 1);
    assert_eq!(episode["ParentId"], series_id);
    assert_eq!(episode["SeriesName"], "Show");
    assert_eq!(episode["People"][0]["Name"], "Series Actor");
    assert_eq!(episode["People"][0]["Role"], "Lead");
}

/// PlaybackInfo 提供缓存章节、marker 与媒体流。
async fn playback_info_serves_cached_chapters_and_streams(fx: &TvArtworkFixture) {
    let app = fx.app.clone();
    let episode_id = fx.episode_id.clone();
    let playback_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/Items/{episode_id}/PlaybackInfo"))
                .header("authorization", "Bearer admin-token")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    let playback = json(playback_response).await;
    assert_eq!(playback["Chapters"].as_array().unwrap().len(), 3);
    assert_eq!(playback["Chapters"][0]["Name"], "Previously cached chapter");
    assert_eq!(playback["Chapters"][1]["MarkerType"], "IntroStart");
    assert_eq!(playback["Chapters"][2]["MarkerType"], "CreditsStart");
    let playback = playback["MediaSources"][0].clone();
    assert_eq!(playback["RunTimeTicks"], 6_000_000_000i64);
    assert_eq!(playback["MediaStreams"][0]["Type"], "Video");
    assert_eq!(playback["MediaStreams"][0]["Codec"], "h264");
    assert_eq!(playback["MediaStreams"][1]["Type"], "Audio");
    assert_eq!(playback["MediaStreams"][1]["Channels"], 2);
    assert_eq!(playback["MediaStreams"][2]["Type"], "Subtitle");
}

/// 图片路由本地直出，远程 still 走跳转。
async fn image_routes_serve_local_artwork_and_redirect_remote_stills(fx: &TvArtworkFixture) {
    let app = fx.app.clone();
    let series_id = fx.series_id.clone();
    let episode_id = fx.episode_id.clone();
    let backdrop = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/Items/{series_id}/Images/Backdrop/0"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(backdrop.status(), StatusCode::OK);
    assert_eq!(
        &to_bytes(backdrop.into_body(), 32).await.unwrap()[..],
        b"fanart"
    );

    let episode_image = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/Items/{episode_id}/Images/Primary"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(episode_image.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        episode_image.headers()["location"],
        "https://image.tmdb.org/t/p/w300/still.jpg"
    );
}

#[tokio::test]
async fn tv_items_include_nfo_artwork_metadata_and_cached_playback_streams() {
    let fx = tv_artwork_fixture().await;
    series_detail_carries_nfo_metadata_and_poster_tags(&fx).await;
    series_marks_round_trip_through_user_data(&fx).await;
    episode_detail_carries_nfo_title_and_people(&fx).await;
    playback_info_serves_cached_chapters_and_streams(&fx).await;
    image_routes_serve_local_artwork_and_redirect_remote_stills(&fx).await;
}
