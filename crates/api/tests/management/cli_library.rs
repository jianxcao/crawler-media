use api::Store;
use api::cli::{Command, Transport, run};
use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource};

use super::cli::{RouterTransport, transport};
use axum::{
    Json, Router,
    extract::{Path, Query},
    http::StatusCode,
    routing::get,
};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use store::test_support::{FixtureFile, publish_fixture_file};

#[tokio::test]
async fn cli_directory_lists_roots_of_libraries_hidden_from_admin_browse() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let root = tmp.path().join("hidden-library");
    std::fs::create_dir(&root).unwrap();
    let (status, library) = t
        .send(
            "POST",
            "/api/v1/libraries",
            Some(serde_json::json!({
                "name": "Hidden",
                "kind": "movie",
                "root_paths": [root],
                "admin_visible": false,
            })),
        )
        .await
        .unwrap();
    assert_eq!(status, 200, "{library}");
    let library_id = library["id"].as_str().unwrap();
    let path = library["root_paths"][0].as_str().unwrap();

    let out = run(Command::Directory, &t).await.unwrap();
    assert!(
        out.lines()
            .any(|line| line == format!("library={library_id} kind=movie root={path}")),
        "{out}"
    );
}

#[tokio::test]
async fn cli_library_prints_no_media_when_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(Command::Library, &t).await.unwrap();
    assert!(out.contains("No Media"), "{out}");
}

#[tokio::test]
async fn cli_library_prints_media_title() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: Some(1999),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    publish_fixture_file(
        &store,
        &FixtureFile {
            file_id: Some(LedgerId::new()),
            media_id: media.id,
            path: tmp
                .path()
                .join("data/library/movies/The Matrix.mkv")
                .display()
                .to_string(),
            season: None,
            episode: None,
            resolution: Some("2160p".into()),
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
            ..Default::default()
        },
    )
    .unwrap();
    drop(store);
    let t = transport(&tmp);
    let out = run(Command::Library, &t).await.unwrap();
    assert!(out.contains("The Matrix"), "{out}");
    assert!(out.contains("movie"), "{out}");
}

#[tokio::test]
async fn cli_library_prints_all_501_titles_exactly_once() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    for index in 0..501 {
        let title = format!("Movie {index:03}");
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: title.clone(),
            year: Some(2024),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&media).unwrap();
        publish_fixture_file(
            &store,
            &FixtureFile {
                media_id: media.id,
                path: tmp
                    .path()
                    .join(format!("data/library/movies/{title}.mkv"))
                    .display()
                    .to_string(),
                ..Default::default()
            },
        )
        .unwrap();
    }
    drop(store);
    let out = run(Command::Library, &transport(&tmp)).await.unwrap();
    let lines: Vec<_> = out.lines().collect();
    assert_eq!(lines.len(), 501);
    for index in 0..501 {
        let expected = format!("Movie {index:03}\tmovie");
        assert_eq!(
            lines.iter().filter(|line| **line == expected).count(),
            1,
            "{expected}"
        );
    }
}

fn paged_transport(pages: Vec<(&str, Option<&str>, u16, Value)>) -> RouterTransport {
    let libraries: BTreeSet<_> = pages.iter().map(|(id, _, _, _)| *id).collect();
    let libraries: Vec<_> = libraries
        .into_iter()
        .map(|id| json!({"id": id, "kind": "movie"}))
        .collect();
    let pages: HashMap<_, _> = pages
        .into_iter()
        .map(|(id, cursor, status, body)| {
            ((id.to_owned(), cursor.map(str::to_owned)), (status, body))
        })
        .collect();
    let pages = Arc::new(pages);
    let app = Router::new()
        .route(
            "/api/v1/libraries",
            get(move || {
                let libraries = libraries.clone();
                async move { Json(json!({"ok": true, "data": libraries})) }
            }),
        )
        .route(
            "/api/v1/libraries/{id}/items",
            get(
                move |Path(id): Path<String>, Query(query): Query<HashMap<String, String>>| {
                    let pages = Arc::clone(&pages);
                    async move {
                        tokio::task::yield_now().await;
                        let (status, body) = pages
                            .get(&(id, query.get("cursor").cloned()))
                            .cloned()
                            .unwrap_or((
                                400,
                                json!({"ok": false, "error": {
                                    "code": "library.invalid", "message": "unexpected cursor"
                                }}),
                            ));
                        (StatusCode::from_u16(status).unwrap(), Json(body))
                    }
                },
            ),
        );
    RouterTransport {
        app,
        token: "management-secret".into(),
    }
}

#[tokio::test]
async fn cli_library_follows_opaque_cursors_encoded_once() {
    let t = paged_transport(vec![
        (
            "movie",
            None,
            200,
            json!({
                "ok": true, "data": [{"title": "First", "kind": "movie"}],
                "next_cursor": "opaque+&%2B /?=汉", "total": 2
            }),
        ),
        (
            "movie",
            Some("opaque+&%2B /?=汉"),
            200,
            json!({
                "ok": true, "data": [{"title": "Second", "kind": "movie"}],
                "next_cursor": null, "total": null
            }),
        ),
    ]);
    assert_eq!(
        run(Command::Library, &t).await.unwrap(),
        "First\tmovie\nSecond\tmovie\n"
    );
}

async fn http_library_cli(t: RouterTransport) -> std::process::Output {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, t.app).await.unwrap() });
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new(env!("CARGO_BIN_EXE_crawler-media"))
            .arg("library")
            .env_clear()
            .env("CRAWLER_MEDIA_URL", format!("http://{address}"))
            .env("CRAWLER_MEDIA_TOKEN", "cli-test-token")
            .env("TOKIO_WORKER_THREADS", "1")
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    server.abort();
    let _ = server.await;
    output
}

#[tokio::test]
async fn cli_library_http_follows_opaque_cursor() {
    let output = http_library_cli(paged_transport(vec![
        (
            "movie",
            None,
            200,
            json!({
                "ok": true, "data": [{"title": "First"}],
                "next_cursor": "opaque+&%2B /?=汉", "total": 2
            }),
        ),
        (
            "movie",
            Some("opaque+&%2B /?=汉"),
            200,
            json!({
                "ok": true, "data": [{"title": "Second"}],
                "next_cursor": null, "total": null
            }),
        ),
    ]))
    .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "First\tmovie\nSecond\tmovie\n"
    );
}

#[tokio::test]
async fn cli_library_http_preserves_later_page_error() {
    let output = http_library_cli(paged_transport(vec![
        (
            "movie",
            None,
            200,
            json!({
                "ok": true, "data": [{"title": "First"}], "next_cursor": "next", "total": 2
            }),
        ),
        (
            "movie",
            Some("next"),
            503,
            json!({"ok": false, "error": {
                "code": "library.unavailable", "message": "page unavailable"
            }}),
        ),
    ]))
    .await;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("503"), "{error}");
    assert!(error.contains("page unavailable"), "{error}");
}

#[tokio::test]
async fn cli_library_starts_each_library_without_a_cursor() {
    let mut pages = Vec::new();
    for id in ["first", "second"] {
        pages.push((
            id,
            None,
            200,
            json!({
                "ok": true, "data": [{"title": format!("{id} start")}],
                "next_cursor": "shared", "total": 2
            }),
        ));
        pages.push((
            id,
            Some("shared"),
            200,
            json!({
                "ok": true, "data": [{"title": format!("{id} end")}],
                "next_cursor": null, "total": null
            }),
        ));
    }
    assert_eq!(
        run(Command::Library, &paged_transport(pages))
            .await
            .unwrap(),
        "first start\tmovie\nfirst end\tmovie\nsecond start\tmovie\nsecond end\tmovie\n"
    );
}

#[tokio::test]
async fn cli_library_returns_page_errors_instead_of_partial_output() {
    for cursor in [None, Some("next")] {
        let mut pages = vec![(
            "movie",
            None,
            200,
            json!({
                "ok": true, "data": [{"title": "First"}], "next_cursor": "next", "total": 2
            }),
        )];
        pages.push((
            "movie",
            cursor,
            503,
            json!({"ok": false, "error": {
                "code": "library.unavailable", "message": "page unavailable"
            }}),
        ));
        let error = run(Command::Library, &paged_transport(pages))
            .await
            .unwrap_err();
        assert!(error.contains("503"), "{error}");
        assert!(error.contains("page unavailable"), "{error}");
    }
}

#[tokio::test]
async fn cli_library_rejects_malformed_pages_and_invalid_cursors() {
    for body in [
        json!({"ok": true, "data": [], "total": 0}),
        json!({"ok": true, "data": [], "next_cursor": "", "total": 0}),
        json!({"ok": true, "data": [], "next_cursor": 12, "total": 0}),
        json!({"ok": true, "data": [], "next_cursor": false, "total": 0}),
        json!({"ok": true, "data": [], "next_cursor": [], "total": 0}),
        json!({"ok": true, "data": [], "next_cursor": {}, "total": 0}),
        json!({"ok": true, "data": {}, "next_cursor": null, "total": 0}),
        json!({"ok": true, "next_cursor": null, "total": 0}),
        json!({"ok": false, "data": [], "next_cursor": null, "total": 0}),
        json!({"data": [], "next_cursor": null, "total": 0}),
        json!([]),
    ] {
        let t = paged_transport(vec![("movie", None, 200, body.clone())]);
        assert!(
            run(Command::Library, &t).await.is_err(),
            "accepted malformed page: {body}"
        );
    }
}

#[tokio::test]
async fn cli_library_rejects_repeated_cursors_and_cursor_cycles() {
    for cycle in [false, true] {
        let mut pages = vec![
            (
                "movie",
                None,
                200,
                json!({
                    "ok": true, "data": [{"title": "First"}], "next_cursor": "a", "total": 3
                }),
            ),
            (
                "movie",
                Some("a"),
                200,
                json!({
                    "ok": true, "data": [{"title": "Second"}],
                    "next_cursor": if cycle { "b" } else { "a" }, "total": null
                }),
            ),
        ];
        if cycle {
            pages.push((
                "movie",
                Some("b"),
                200,
                json!({
                    "ok": true, "data": [{"title": "Third"}], "next_cursor": "a", "total": null
                }),
            ));
        }
        let t = paged_transport(pages);
        let error =
            tokio::time::timeout(std::time::Duration::from_secs(2), run(Command::Library, &t))
                .await
                .expect("cursor loops must terminate")
                .unwrap_err();
        assert!(error.contains("cursor"), "{error}");
    }
}

#[tokio::test]
async fn cli_ledger_prints_no_ledger_when_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(Command::Ledger, &t).await.unwrap();
    assert!(out.contains("No ledger"), "{out}");
}

#[tokio::test]
async fn cli_ledger_prints_media_title_and_path() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: Some(1999),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    publish_fixture_file(
        &store,
        &FixtureFile {
            file_id: Some(LedgerId::new()),
            media_id: media.id,
            path: tmp
                .path()
                .join("data/library/movies/The Matrix.mkv")
                .display()
                .to_string(),
            season: None,
            episode: None,
            resolution: Some("2160p".into()),
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
            ..Default::default()
        },
    )
    .unwrap();
    drop(store);
    let t = transport(&tmp);
    let out = run(Command::Ledger, &t).await.unwrap();
    assert!(out.contains("The Matrix"), "{out}");
    assert!(out.contains("The Matrix.mkv"), "{out}");
}

#[tokio::test]
async fn cli_unidentified_prints_no_unidentified_when_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(Command::Unidentified, &t).await.unwrap();
    assert!(out.contains("No Unidentified"), "{out}");
}

#[tokio::test]
async fn cli_unidentified_prints_parked_path() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("unidentified/foo.mkv");
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .insert_unidentified(path.display().to_string(), Confidence::Low)
        .unwrap();
    drop(store);
    let t = transport(&tmp);
    let out = run(Command::Unidentified, &t).await.unwrap();
    assert!(out.contains(&path.display().to_string()), "{out}");
    assert!(out.contains("low"), "{out}");
}

#[tokio::test]
async fn cli_unidentified_claim_prints_media_title() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let intake = tmp.path().join("intake");
    std::fs::create_dir_all(&intake).unwrap();
    let src = intake.join("foo.mkv");
    std::fs::write(&src, b"video").unwrap();
    store
        .insert_unidentified(src.display().to_string(), Confidence::Low)
        .unwrap();
    drop(store);
    let t = transport(&tmp);
    let out = run(
        Command::ClaimUnidentified {
            path: src.display().to_string(),
            title: "The Matrix".into(),
            kind: "movie".into(),
            year: Some(1999),
        },
        &t,
    )
    .await
    .unwrap();
    assert!(out.contains("The Matrix"), "{out}");
    let listed = run(Command::Unidentified, &t).await.unwrap();
    assert!(listed.contains("No Unidentified"), "{listed}");
}
