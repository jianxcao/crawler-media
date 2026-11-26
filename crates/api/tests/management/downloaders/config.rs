use super::*;

use api::{ChosenDownloader, DownloaderEnv, Store, choose_downloader};
use axum::http::StatusCode;
use serde_json::{Value, json};
use tower::ServiceExt;

#[tokio::test]
async fn two_downloaders_one_default_and_list_omits_password() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let first = post_dl(
        &app,
        json!({
            "name": "qb-a",
            "kind": "qbittorrent",
            "url": "http://qb-a:8080",
            "username": "admin",
            "password": "secret-a",
            "is_default": true
        }),
    )
    .await;
    let second = post_dl(
        &app,
        json!({
            "name": "qb-b",
            "kind": "qbittorrent",
            "url": "http://qb-b:8080",
            "username": "admin",
            "password": "secret-b",
            "is_default": true
        }),
    )
    .await;
    assert_ne!(first["id"], second["id"]);

    let listed = app
        .oneshot(request(
            "GET",
            "/api/v1/downloaders",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let rows = json_data(listed).await;
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let a = rows.iter().find(|row| row["name"] == "qb-a").unwrap();
    let b = rows.iter().find(|row| row["name"] == "qb-b").unwrap();
    assert_eq!(a["is_default"], false);
    assert_eq!(b["is_default"], true);
    assert!(a.get("password").is_none());
    assert!(b.get("password").is_none());

    let store = Store::open(tmp.path().join("data")).unwrap();
    let ChosenDownloader::Qbittorrent(cfg) =
        choose_downloader(&store, &DownloaderEnv::default()).unwrap()
    else {
        panic!("default row");
    };
    assert_eq!(cfg.url, "http://qb-b:8080");
    assert_eq!(cfg.password, "secret-b");
}

async fn assert_default_states(app: axum::Router) {
    let listed = json_data(
        app.oneshot(request(
            "GET",
            "/api/v1/downloaders",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    let a = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "qb-a")
        .unwrap();
    let b = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "qb-b")
        .unwrap();
    assert_eq!(a["is_default"], true);
    assert_eq!(b["is_default"], false);
}

#[tokio::test]
async fn patch_makes_existing_downloader_the_default() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let first = post_dl(
        &app,
        json!({
            "name": "qb-a",
            "kind": "qbittorrent",
            "url": "http://qb-a:8080",
            "username": "admin",
            "password": "a",
            "is_default": true
        }),
    )
    .await;
    post_dl(
        &app,
        json!({
            "name": "qb-b",
            "kind": "qbittorrent",
            "url": "http://qb-b:8080",
            "username": "admin",
            "password": "b",
            "is_default": false
        }),
    )
    .await;
    let id = first["id"].as_str().unwrap();
    let patched = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/downloaders/{id}"),
            Some("management-secret"),
            json!({ "is_default": true }),
        ))
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);
    assert_default_states(app).await;
}
#[tokio::test]
async fn downloader_enabled_flag_round_trips_through_patch() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let created = post_dl(
        &app,
        json!({
            "name": "qb-a",
            "kind": "qbittorrent",
            "url": "http://qb-a:8080",
            "username": "admin",
            "password": "secret-a",
            "is_default": true
        }),
    )
    .await;
    assert_eq!(created["enabled"], true, "新建的下载器默认启用");
    let id = created["id"].as_str().unwrap().to_string();

    let response = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/downloaders/{id}"),
            Some("management-secret"),
            json!({ "name": "qb-a", "enabled": false }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{response:?}");
    assert_eq!(json_data(response).await["enabled"], false);
}

#[tokio::test]
async fn patch_downloader_rejects_invalid_kind_without_saving() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let created = post_dl(
        &app,
        json!({
            "name": "qb-a",
            "kind": "qbittorrent",
            "url": "http://qb-a:8080",
            "username": "admin",
            "password": "secret-a",
            "is_default": true
        }),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();

    for bad_kind in ["aria2", "   "] {
        let resp = app
            .clone()
            .oneshot(request(
                "PATCH",
                &format!("/api/v1/downloaders/{id}"),
                Some("management-secret"),
                json!({ "kind": bad_kind }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "kind={bad_kind}");
        assert_eq!(json_error(resp).await["code"], "downloader.invalid");
    }

    let detail = json_data(
        app.oneshot(request(
            "GET",
            &format!("/api/v1/downloaders/{id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(detail["kind"], "qbittorrent");
}

#[tokio::test]
async fn api_key_downloader_masks_secrets_selects_bearer_and_toggles_via_patch() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let created = post_dl(
        &app,
        json!({
            "name": "qb-apikey",
            "kind": "qbittorrent",
            "url": "http://qb-v5:8080",
            "api_key": "  qbt_test_key_123  ",
            "path_maps": [{ "from": "/downloads", "to": "/host/qb" }],
            "is_default": true
        }),
    )
    .await;
    assert_eq!(created["auth_type"], "apikey");
    assert_eq!(created["has_api_key"], true);
    assert_eq!(created["has_password"], false);
    assert!(created.get("api_key").is_none());
    assert!(created.get("password").is_none());
    let id = created["id"].as_str().unwrap().to_string();

    let store = Store::open(tmp.path().join("data")).unwrap();
    let ChosenDownloader::Qbittorrent(cfg) =
        choose_downloader(&store, &DownloaderEnv::default()).unwrap()
    else {
        panic!("expected qbittorrent config");
    };
    assert_eq!(cfg.api_key.as_deref(), Some("qbt_test_key_123"));
    assert_eq!(cfg.username, "");
    assert_eq!(cfg.password, "");

    // Partial PATCH omitting api_key keeps stored api_key intact.
    let renamed = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/downloaders/{id}"),
            Some("management-secret"),
            json!({ "name": "qb-apikey-renamed" }),
        ))
        .await
        .unwrap();
    assert_eq!(renamed.status(), StatusCode::OK);
    let renamed_data = json_data(renamed).await;
    assert_eq!(renamed_data["auth_type"], "apikey");
    assert_eq!(renamed_data["has_api_key"], true);
    assert!(renamed_data.get("api_key").is_none());

    // Switching back to password mode clears api_key when api_key="" is sent.
    let switched = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/downloaders/{id}"),
            Some("management-secret"),
            json!({ "username": "admin", "password": "pw", "api_key": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(switched.status(), StatusCode::OK);
    let switched_data = json_data(switched).await;
    assert_eq!(switched_data["auth_type"], "password");
    assert_eq!(switched_data["has_api_key"], false);
    assert_eq!(switched_data["has_password"], true);

    let ChosenDownloader::Qbittorrent(cfg) =
        choose_downloader(&store, &DownloaderEnv::default()).unwrap()
    else {
        panic!("expected qbittorrent config");
    };
    assert_eq!(cfg.api_key, None);
    assert_eq!(cfg.username, "admin");
    assert_eq!(cfg.password, "pw");
}

#[tokio::test]
async fn verify_api_key_downloader_succeeds_via_bearer_without_login() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let shutdown = Arc::new(AtomicBool::new(false));
    let login_hit = Arc::new(AtomicBool::new(false));
    let flag = shutdown.clone();
    let login_flag = login_hit.clone();
    let server = std::thread::spawn(move || {
        while !flag.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut buf = [0u8; 4096];
                    let n = stream.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let lower = req.to_ascii_lowercase();
                    let (status, body) = if req.contains("/api/v2/auth/login") {
                        login_flag.store(true, Ordering::Relaxed);
                        ("500 Internal Server Error", "must not login")
                    } else if req.contains("/api/v2/app/version")
                        && lower.contains("authorization: bearer qbt_live_verify")
                    {
                        ("200 OK", "v5.3.0")
                    } else {
                        ("401 Unauthorized", "missing bearer")
                    };
                    let resp = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });

    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let created = post_dl(
        &app,
        json!({
            "name": "qb-bearer",
            "kind": "qbittorrent",
            "url": format!("http://{addr}"),
            "api_key": "qbt_live_verify",
            "is_default": true
        }),
    )
    .await;
    let id = created["id"].as_str().unwrap();

    let verified = app
        .oneshot(request(
            "POST",
            &format!("/api/v1/downloaders/{id}/verify"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    shutdown.store(true, Ordering::Relaxed);
    let _ = server.join();
    assert_eq!(verified.status(), StatusCode::OK);
    let data = json_data(verified).await;
    assert_eq!(data["ok"], true, "{data}");
    assert!(
        !login_hit.load(Ordering::Relaxed),
        "must not call /auth/login"
    );
}

#[tokio::test]
async fn switching_qbittorrent_to_transmission_or_creating_transmission_clears_api_key() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let store = Store::open(tmp.path().join("data")).unwrap();

    let tr = post_dl(
        &app,
        json!({
            "name": "tr-direct",
            "kind": "transmission",
            "url": "http://127.0.0.1:9091/transmission/rpc",
            "api_key": "qbt_should_not_persist"
        }),
    )
    .await;
    assert_eq!(tr["has_api_key"], false);
    assert_eq!(tr["auth_type"], "password");
    let tr_id = tr["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(store.get_downloader(tr_id).unwrap().unwrap().api_key, None);

    let qb = post_dl(
        &app,
        json!({
            "name": "qb-to-tr",
            "kind": "qbittorrent",
            "url": "http://127.0.0.1:8080",
            "api_key": "qbt_valid_secret"
        }),
    )
    .await;
    assert_eq!(qb["has_api_key"], true);
    let qb_id_str = qb["id"].as_str().unwrap().to_string();
    let qb_id = qb_id_str.parse().unwrap();

    let patched = app
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/downloaders/{qb_id_str}"),
            Some("management-secret"),
            json!({ "kind": "transmission" }),
        ))
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);
    let patched_data = json_data(patched).await;
    assert_eq!(patched_data["has_api_key"], false);
    assert_eq!(patched_data["auth_type"], "password");
    assert_eq!(store.get_downloader(qb_id).unwrap().unwrap().api_key, None);
}
