use super::*;

#[tokio::test]
async fn nfo_people_are_clickable_and_filter_related_items() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, item_id) = app(tmp.path());
    let remaining_cast = (1..33)
        .map(|number| {
            format!(
                "<actor><name>Cast Member {number}</name><role>Character {number}</role><tmdbid>{}</tmdbid></actor>",
                6384 + number
            )
        })
        .collect::<String>();
    std::fs::write(
        tmp.path().join("matrix.nfo"),
        format!(
            "<movie><title>The Matrix</title><year>1999</year><plot>Neo wakes up.</plot><actor><name>Keanu Reeves</name><role>Neo</role><tmdbid>6384</tmdbid><thumb>/profile.jpg</thumb></actor>{remaining_cast}</movie>"
        ),
    )
    .unwrap();

    let store = Store::open(tmp.path().join("data")).unwrap();
    let ledger_key = uuid::Uuid::parse_str(&item_id).unwrap().to_string();
    store
        .put_file_meta(
            &ledger_key,
            &library::Tracks {
                video: Some(library::VideoTrack {
                    stream_index: Some(0),
                    codec: Some("hevc".into()),
                    profile: Some("Main 10".into()),
                    width: Some(3840),
                    height: Some(1600),
                    frame_rate: Some(23.976),
                    average_frame_rate: Some(23.976),
                    bit_rate: Some(18_000_000),
                    duration_secs: Some(7200.0),
                    aspect_ratio: Some("12:5".into()),
                    pixel_format: Some("yuv420p10le".into()),
                    bit_depth: Some(10),
                    color_space: Some("bt2020nc".into()),
                    color_transfer: Some("smpte2084".into()),
                    ..Default::default()
                }),
                audio: vec![library::AudioTrack {
                    stream_index: Some(1),
                    codec: Some("eac3".into()),
                    profile: Some("Dolby Digital Plus".into()),
                    channels: Some(6),
                    channel_layout: Some("5.1(side)".into()),
                    language: Some("ja".into()),
                    title: Some("Japanese 5.1".into()),
                    sample_rate: Some("48000".into()),
                    bit_rate: Some(768_000),
                    is_default: true,
                    ..Default::default()
                }],
                subtitles: vec![library::SubtitleTrack {
                    stream_index: Some(2),
                    codec: Some("ass".into()),
                    language: Some("zh".into()),
                    title: Some("简体中文".into()),
                    is_default: false,
                    forced: true,
                    ..Default::default()
                }],
            },
        )
        .unwrap();
    std::fs::write(tmp.path().join("matrix.en.forced.srt"), "subtitle").unwrap();

    let item = get(&app, &format!("/Items/{item_id}")).await;
    assert_eq!(item["People"].as_array().unwrap().len(), 33);
    let person = &item["People"][0];
    assert_eq!(person["Name"], "Keanu Reeves");
    assert_eq!(person["Role"], "Neo");
    assert_eq!(person["Type"], "Actor");
    assert_eq!(person["ProviderIds"]["Tmdb"], "6384");
    assert_eq!(item["MediaStreams"][0]["Profile"], "Main 10");
    assert_eq!(item["MediaStreams"][0]["BitDepth"], 10);
    assert_eq!(item["MediaStreams"][0]["IsDefault"], false);
    assert_eq!(item["MediaStreams"][0]["ColorTransfer"], "smpte2084");
    assert_eq!(item["MediaStreams"][1]["Language"], "ja");
    assert_eq!(item["MediaStreams"][1]["ChannelLayout"], "5.1(side)");
    assert_eq!(item["MediaStreams"][1]["BitRate"], 768_000);
    assert_eq!(item["MediaStreams"][2]["Title"], "简体中文");
    assert_eq!(item["MediaStreams"][3]["IsExternal"], true);
    assert_eq!(item["MediaStreams"][3]["Language"], "en");
    assert_eq!(item["MediaStreams"][3]["IsForced"], true);
    assert_eq!(
        item["MediaStreams"][3]["Path"],
        tmp.path()
            .join("matrix.en.forced.srt")
            .display()
            .to_string()
    );
    let person_id = person["Id"].as_str().unwrap();
    assert!(!person_id.is_empty());

    let person_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/Persons/Keanu%20Reeves")
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(person_response.status(), StatusCode::OK);
    let person_details = json(person_response).await;
    assert_eq!(person_details["Id"], person_id);
    assert_eq!(person_details["Name"], "Keanu Reeves");
    assert_eq!(person_details["Type"], "Person");

    let image_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/Persons/Keanu%20Reeves/Images/Primary")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(image_response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        image_response.headers()["location"],
        "https://image.tmdb.org/t/p/w185/profile.jpg"
    );

    let matches = get(&app, &format!("/Items?PersonIds={person_id}")).await;
    assert_eq!(matches["TotalRecordCount"], 1);
    assert_eq!(matches["Items"][0]["Id"], item_id);
}

async fn get(app: &axum::Router, uri: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{uri}");
    json(response).await
}
