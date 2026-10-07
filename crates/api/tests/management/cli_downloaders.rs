use api::cli::{Command, Transport, run};
use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;

use super::cli::transport;
use super::common::*;

#[tokio::test]
async fn cli_downloaders_prints_no_downloaders_when_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(Command::Downloaders, &t).await.unwrap();
    assert_eq!(out, "No Downloaders\n");
}

#[tokio::test]
async fn cli_downloaders_prints_name_and_omits_password() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    t.app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/downloaders",
            Some("management-secret"),
            json!({
                "name": "qb-a",
                "kind": "qbittorrent",
                "url": "http://qb-a:8080",
                "username": "admin",
                "password": "secret-a",
                "is_default": true
            }),
        ))
        .await
        .unwrap();
    let out = run(Command::Downloaders, &t).await.unwrap();
    assert!(out.contains("qb-a"), "{out}");
    assert!(out.contains("qbittorrent"), "{out}");
    assert!(out.contains("http://qb-a:8080"), "{out}");
    assert!(out.contains("default"), "{out}");
    assert!(!out.contains("secret-a"), "{out}");
}

#[tokio::test]
async fn cli_downloaders_add_prints_name_and_omits_password() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(
        Command::DownloadersAdd {
            name: "qb-a".into(),
            kind: "qbittorrent".into(),
            url: "http://qb-a:8080".into(),
            username: Some("admin".into()),
            password: Some("secret-a".into()),
            is_default: true,
        },
        &t,
    )
    .await
    .unwrap();
    assert!(out.contains("qb-a"), "{out}");
    assert!(out.contains("qbittorrent"), "{out}");
    assert!(out.contains("http://qb-a:8080"), "{out}");
    assert!(out.contains("default"), "{out}");
    assert!(!out.contains("secret-a"), "{out}");
    let listed = run(Command::Downloaders, &t).await.unwrap();
    assert!(listed.contains("qb-a"), "{listed}");
    assert!(!listed.contains("secret-a"), "{listed}");
}

#[tokio::test]
async fn cli_downloaders_default_prints_default_true() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    run(
        Command::DownloadersAdd {
            name: "qb-a".into(),
            kind: "qbittorrent".into(),
            url: "http://qb-a:8080".into(),
            username: None,
            password: None,
            is_default: true,
        },
        &t,
    )
    .await
    .unwrap();
    run(
        Command::DownloadersAdd {
            name: "qb-b".into(),
            kind: "qbittorrent".into(),
            url: "http://qb-b:8080".into(),
            username: None,
            password: None,
            is_default: false,
        },
        &t,
    )
    .await
    .unwrap();
    let listed = t
        .app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/downloaders",
            Some("management-secret"),
            serde_json::Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let id = json_body(listed).await["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "qb-b")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let out = run(Command::DownloadersDefault { id }, &t).await.unwrap();
    assert!(out.contains("default=true"), "{out}");
    let listed = run(Command::Downloaders, &t).await.unwrap();
    assert!(listed.contains("qb-b"), "{listed}");
    assert!(listed.contains("default"), "{listed}");
}
#[tokio::test]
async fn cli_directory_prints_default_library_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(Command::Directory, &t).await.unwrap();
    assert!(out.contains("movie_root="), "{out}");
    assert!(out.contains("tv_root="), "{out}");
    assert!(out.contains("movies"), "{out}");
}

#[tokio::test]
async fn cli_directory_prints_extra_library_root() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let extra = tmp.path().join("disk2/movies");
    let listed = t.send("GET", "/api/v1/directory", None).await.unwrap();
    assert_eq!(listed.0, 200);
    t.send(
        "POST",
        "/api/v1/directory/roots",
        Some(json!({
            "kind": "movie",
            "path": extra.display().to_string()
        })),
    )
    .await
    .unwrap();
    let out = run(Command::Directory, &t).await.unwrap();
    assert!(out.contains(&extra.display().to_string()), "{out}");
}

#[tokio::test]
async fn cli_directory_add_root_prints_extra_path() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let extra = tmp.path().join("disk2/movies");
    let out = run(
        Command::DirectoryAddRoot {
            kind: "movie".into(),
            path: extra.display().to_string(),
        },
        &t,
    )
    .await
    .unwrap();
    assert!(out.contains(&extra.display().to_string()), "{out}");
    let listed = run(Command::Directory, &t).await.unwrap();
    assert!(listed.contains(&extra.display().to_string()), "{listed}");
}

#[tokio::test]
async fn cli_directory_remove_root_drops_extra_path() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let extra = tmp.path().join("disk2/movies");
    run(
        Command::DirectoryAddRoot {
            kind: "movie".into(),
            path: extra.display().to_string(),
        },
        &t,
    )
    .await
    .unwrap();
    let listed = t.send("GET", "/api/v1/directory", None).await.unwrap();
    let id = listed.1["extra_roots"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let out = run(Command::DirectoryRemoveRoot { id }, &t).await.unwrap();
    assert!(out.contains("deleted="), "{out}");
    let after = run(Command::Directory, &t).await.unwrap();
    assert!(!after.contains(&extra.display().to_string()), "{after}");
}

#[tokio::test]
async fn cli_directory_sets_watch_inplace() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let watch = tmp.path().join("existing");
    let out = run(
        Command::DirectoryWatchInplace {
            path: watch.display().to_string(),
        },
        &t,
    )
    .await
    .unwrap();
    assert!(
        out.contains(&format!("watch_inplace={}", watch.display())),
        "{out}"
    );
    let listed = run(Command::Directory, &t).await.unwrap();
    assert!(
        listed.contains(&format!("watch_inplace={}", watch.display())),
        "{listed}"
    );
}

#[tokio::test]
async fn cli_directory_sets_watch_intake() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let watch = tmp.path().join("drops");
    let out = run(
        Command::DirectoryWatchIntake {
            path: watch.display().to_string(),
        },
        &t,
    )
    .await
    .unwrap();
    assert!(
        out.contains(&format!("watch_intake={}", watch.display())),
        "{out}"
    );
    let listed = run(Command::Directory, &t).await.unwrap();
    assert!(
        listed.contains(&format!("watch_intake={}", watch.display())),
        "{listed}"
    );
}

#[tokio::test]
async fn cli_directory_sets_transfer_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(
        Command::DirectoryTransferMode {
            mode: "copy".into(),
        },
        &t,
    )
    .await
    .unwrap();
    assert!(out.contains("transfer_mode=copy"), "{out}");
    let listed = run(Command::Directory, &t).await.unwrap();
    assert!(listed.contains("transfer_mode=copy"), "{listed}");
}

#[tokio::test]
async fn cli_directory_sets_scrape() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(Command::DirectoryScrape { enabled: true }, &t)
        .await
        .unwrap();
    assert!(out.contains("scrape=true"), "{out}");
    let listed = run(Command::Directory, &t).await.unwrap();
    assert!(listed.contains("scrape=true"), "{listed}");
}

#[tokio::test]
async fn cli_directory_sets_movie_naming() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(
        Command::DirectoryMovieNaming {
            pattern: "{title} ({year})/{title}".into(),
        },
        &t,
    )
    .await
    .unwrap();
    assert!(
        out.contains("movie_naming={title} ({year})/{title}"),
        "{out}"
    );
    let listed = run(Command::Directory, &t).await.unwrap();
    assert!(
        listed.contains("movie_naming={title} ({year})/{title}"),
        "{listed}"
    );
}

#[tokio::test]
async fn cli_directory_sets_tv_naming() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(
        Command::DirectoryTvNaming {
            pattern: "{title}/Season {season}/{title} - S{season}E{episode}".into(),
        },
        &t,
    )
    .await
    .unwrap();
    assert!(
        out.contains("tv_naming={title}/Season {season}/{title} - S{season}E{episode}"),
        "{out}"
    );
    let listed = run(Command::Directory, &t).await.unwrap();
    assert!(
        listed.contains("tv_naming={title}/Season {season}/{title} - S{season}E{episode}"),
        "{listed}"
    );
}
