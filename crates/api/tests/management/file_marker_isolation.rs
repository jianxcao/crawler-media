use std::{collections::HashMap, sync::Arc};

use axum::http::StatusCode;
use domain::{Media, MediaId, MediaKind, UnitKey};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

use super::common::*;

fn seed_cut(
    store: &api::Store,
    root: &std::path::Path,
    media: MediaId,
    start: i64,
) -> domain::LibraryFile {
    std::fs::create_dir_all(root).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            &format!("Cut {start}"),
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let path = root.join("S01E01.mkv");
    std::fs::write(&path, format!("cut {start}")).unwrap();
    let file = store::test_support::publish_fixture_file_units(
        store,
        &store::test_support::FixtureFile {
            media_id: media,
            path: path.display().to_string(),
            ..Default::default()
        },
        &[UnitKey::new(1, 1)],
    )
    .unwrap();
    let marker = store::StoredFileMarker {
        file_id: file.file_id,
        version: file.version.clone(),
        unit: UnitKey::new(1, 1),
        intro_start_ms: Some(start),
        intro_end_ms: Some(start + 60_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: "manual".into(),
        locked: true,
        updated_at: 0,
    };
    let members = store::observe_sources(&store.source_snapshots(&[file.file_id]).unwrap());
    store.put_file_marker_checked(&marker, &members).unwrap();
    store
        .put_cached_chapters(&file.file_id.to_string(), &[])
        .unwrap();
    file
}

async fn get_json(app: &axum::Router, url: &str) -> Value {
    let response = app
        .clone()
        .oneshot(request("GET", url, Some("management-secret"), Value::Null))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{url}");
    json_body(response).await
}

#[tokio::test]
async fn selected_file_markers_are_isolated_in_chapters_jellyfin_and_playback() {
    let tmp = tempfile::tempdir().unwrap();
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let media = store
        .ensure_media(Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "Two cuts".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let first = seed_cut(&store, &tmp.path().join("first"), media.id, 10_000);
    let second = seed_cut(&store, &tmp.path().join("second"), media.id, 40_000);
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = api::router(state(tmp.path(), fetcher, downloader));
    let wrong = app
        .clone()
        .oneshot(request(
            "GET",
            &format!(
                "/api/v1/libraries/{}/items/{}/chapters/frame-status?file_id={}&season=1&episode=1",
                first.library_id, media.id, second.file_id
            ),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::NOT_FOUND);
    let other_task = store
        .current_chapter_frame_task(second.file_id)
        .unwrap()
        .unwrap();
    assert!(
        !json_body(wrong)
            .await
            .to_string()
            .contains(&other_task.task.id)
    );
    for (file, expected) in [(&first, 10_000), (&second, 40_000)] {
        let chapters = get_json(
            &app,
            &format!(
                "/api/v1/libraries/{}/items/{}/chapters?file_id={}&season=1&episode=1",
                file.library_id, media.id, file.file_id
            ),
        )
        .await;
        let intro = chapters["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["marker_type"] == "IntroStart")
            .unwrap();
        assert_eq!(intro["start_ms"], expected);
        let listing = get_json(
            &app,
            &format!(
                "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Episode",
                file.library_id
            ),
        )
        .await;
        let item = &listing["Items"][0];
        let intro = item["Chapters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["MarkerType"] == "IntroStart")
            .unwrap();
        assert_eq!(intro["StartPositionTicks"], expected * 10_000);
        let session = store
            .issue_play_session(
                store.admin_user_id().unwrap(),
                "isolated-cut",
                file.file_id,
                media.id,
                UnitKey::new(1, 1),
                &file.version,
                0,
            )
            .unwrap();
        let playback = get_json(
            &app,
            &format!("/api/v1/playback/chapters?play_session_id={session}&device_id=isolated-cut"),
        )
        .await;
        let intro = playback["data"]["chapters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["title"] == "片头")
            .unwrap();
        assert_eq!(intro["start_ms"], expected);
    }
}

#[tokio::test]
async fn theintrodb_candidate_without_file_validation_is_not_published_or_overlaid() {
    use std::io::{Read, Write};
    let tmp = tempfile::tempdir().unwrap();
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let media = store
        .ensure_media(Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "Candidate".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: Some("123".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let root = tmp.path().join("strm");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Candidate",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let path = root.join("S01E01.strm");
    std::fs::write(&path, "https://fixture.invalid/video.mkv").unwrap();
    let file = store::test_support::publish_fixture_file_units(
        &store,
        &store::test_support::FixtureFile {
            media_id: media.id,
            path: path.display().to_string(),
            ..Default::default()
        },
        &[UnitKey::new(1, 1)],
    )
    .unwrap();
    store
        .set_library_intro_settings(&file.library_id.to_string(), true, false)
        .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                        .unwrap();
                    let mut request = [0_u8; 4096];
                    let read = stream.read(&mut request).unwrap();
                    assert!(read > 0, "candidate lookup must send an HTTP request");
                    let body = r#"{"segments":[{"type":"intro","start":10,"end":70}]}"#;
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                    return;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "candidate lookup was never requested"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(error) => panic!("candidate fixture failed: {error}"),
            }
        }
    });
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let state = state(tmp.path(), fetcher, downloader);
    let row = store.file_coverage_by_id(file.file_id).unwrap().unwrap();
    let chapters = api::marker_resolver::resolve_item_chapters(
        &state,
        &row,
        UnitKey::new(1, 1),
        &media,
        Some(
            api::theintrodb::TheIntroDbClient::new(Some("fixture-key".into()))
                .with_base_url(format!("http://{address}")),
        ),
    )
    .await;
    server.join().unwrap();
    assert!(
        chapters.is_empty(),
        "unverified candidate cannot become a chapter overlay"
    );
    assert!(
        store
            .get_file_marker(file.file_id, UnitKey::new(1, 1))
            .unwrap()
            .is_none()
    );
}
