use std::collections::HashMap;
use std::sync::Arc;

use api::cli::{Command, Transport, run};
use api::{Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;

pub(super) struct RouterTransport {
    pub(super) app: axum::Router,
    pub(super) token: String,
}

impl Transport for RouterTransport {
    async fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<(u16, Value), String> {
        let mut builder = Request::builder().method(method).uri(path);
        builder = builder.header("authorization", format!("Bearer {}", self.token));
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        let request = builder
            .body(Body::from(
                body.map(|value| value.to_string()).unwrap_or_default(),
            ))
            .map_err(|err| err.to_string())?;
        let response = self
            .app
            .clone()
            .oneshot(request)
            .await
            .map_err(|err| err.to_string())?;
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .map_err(|err| err.to_string())?;
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        let value = match value {
            Value::Object(mut map) if map.get("ok").and_then(Value::as_bool) == Some(true) => {
                map.remove("data").unwrap_or(Value::Null)
            }
            other => other,
        };
        Ok((status, value))
    }
}

pub(super) fn transport(tmp: &tempfile::TempDir) -> RouterTransport {
    search_transport(tmp, HashMap::new())
}

fn search_transport(
    tmp: &tempfile::TempDir,
    bodies: HashMap<&'static str, &'static str>,
) -> RouterTransport {
    RouterTransport {
        app: router(state(
            tmp.path(),
            Arc::new(Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies,
            }),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )),
        token: "management-secret".into(),
    }
}

#[test]
fn empty_argv_is_serve() {
    assert_eq!(Command::parse(&[] as &[&str]).unwrap(), Command::Serve);
}

#[test]
fn sites_add_without_name_is_error() {
    assert!(
        Command::parse(&[
            "sites",
            "--add",
            "--url",
            "https://pt.example/",
            "--profile",
            "demo"
        ])
        .is_err()
    );
}

#[test]
fn sites_disable_without_id_is_error() {
    assert!(Command::parse(&["sites", "--disable"]).is_err());
}

#[test]
fn sites_enable_without_id_is_error() {
    assert!(Command::parse(&["sites", "--enable"]).is_err());
}

#[test]
fn library_extra_arg_is_error() {
    assert!(Command::parse(&["library", "extra"]).is_err());
}

#[test]
fn library_parses() {
    assert_eq!(Command::parse(&["library"]).unwrap(), Command::Library);
}

#[test]
fn ledger_parses() {
    assert_eq!(Command::parse(&["ledger"]).unwrap(), Command::Ledger);
}

#[test]
fn ledger_extra_arg_is_error() {
    assert!(Command::parse(&["ledger", "extra"]).is_err());
}

#[test]
fn directory_extra_arg_is_error() {
    assert!(Command::parse(&["directory", "extra"]).is_err());
}

#[test]
fn directory_parses() {
    assert_eq!(Command::parse(&["directory"]).unwrap(), Command::Directory);
}

#[test]
fn directory_add_root_without_path_is_error() {
    assert!(Command::parse(&["directory", "--add-root", "--kind", "movie"]).is_err());
}

#[test]
fn directory_remove_root_without_id_is_error() {
    assert!(Command::parse(&["directory", "--remove-root"]).is_err());
}

#[test]
fn directory_watch_inplace_without_path_is_error() {
    assert!(Command::parse(&["directory", "--watch-inplace"]).is_err());
}

#[test]
fn directory_watch_intake_without_path_is_error() {
    assert!(Command::parse(&["directory", "--watch-intake"]).is_err());
}

#[test]
fn directory_transfer_mode_without_value_is_error() {
    assert!(Command::parse(&["directory", "--transfer-mode"]).is_err());
}

#[test]
fn directory_transfer_mode_bogus_is_error() {
    assert!(Command::parse(&["directory", "--transfer-mode", "bogus"]).is_err());
}

#[test]
fn directory_scrape_without_value_is_error() {
    assert!(Command::parse(&["directory", "--scrape"]).is_err());
}

#[test]
fn directory_scrape_bogus_is_error() {
    assert!(Command::parse(&["directory", "--scrape", "maybe"]).is_err());
}

#[test]
fn directory_movie_naming_without_value_is_error() {
    assert!(Command::parse(&["directory", "--movie-naming"]).is_err());
}

#[test]
fn directory_tv_naming_without_value_is_error() {
    assert!(Command::parse(&["directory", "--tv-naming"]).is_err());
}

#[test]
fn downloads_extra_arg_is_error() {
    assert!(Command::parse(&["downloads", "extra"]).is_err());
}

#[test]
fn downloads_parses() {
    assert_eq!(Command::parse(&["downloads"]).unwrap(), Command::Downloads);
}

#[test]
fn subscribe_parses_as_list() {
    assert_eq!(Command::parse(&["subscribe"]).unwrap(), Command::Subscribes);
}

#[test]
fn filters_parses() {
    assert_eq!(Command::parse(&["filters"]).unwrap(), Command::Filters);
}

#[test]
fn filters_extra_arg_is_error() {
    assert!(Command::parse(&["filters", "extra"]).is_err());
}

#[test]
fn filters_add_without_name_is_error() {
    assert!(
        Command::parse(&[
            "filters",
            "--add",
            "--atom",
            "resolution=2160p",
            "--priority",
            "100"
        ])
        .is_err()
    );
}

#[test]
fn filters_add_without_atom_is_error() {
    assert!(Command::parse(&["filters", "--add", "--name", "uhd"]).is_err());
}

#[test]
fn filters_default_without_id_is_error() {
    assert!(Command::parse(&["filters", "--default"]).is_err());
}

#[test]
fn downloaders_parses() {
    assert_eq!(
        Command::parse(&["downloaders"]).unwrap(),
        Command::Downloaders
    );
}

#[test]
fn downloaders_extra_arg_is_error() {
    assert!(Command::parse(&["downloaders", "extra"]).is_err());
}

#[test]
fn downloaders_add_without_name_is_error() {
    assert!(
        Command::parse(&[
            "downloaders",
            "--add",
            "--kind",
            "qbittorrent",
            "--url",
            "http://qb-a:8080"
        ])
        .is_err()
    );
}

#[test]
fn downloaders_default_without_id_is_error() {
    assert!(Command::parse(&["downloaders", "--default"]).is_err());
}

#[test]
fn users_parses() {
    assert_eq!(Command::parse(&["users"]).unwrap(), Command::Users);
}

#[test]
fn users_extra_arg_is_error() {
    assert!(Command::parse(&["users", "extra"]).is_err());
}

#[test]
fn users_add_without_login_is_error() {
    assert!(Command::parse(&["users", "--add", "--token", "alice-token"]).is_err());
}
#[test]
fn unidentified_extra_arg_is_error() {
    assert!(Command::parse(&["unidentified", "extra"]).is_err());
}

#[test]
fn unidentified_parses() {
    assert_eq!(
        Command::parse(&["unidentified"]).unwrap(),
        Command::Unidentified
    );
}

#[test]
fn unidentified_claim_without_path_is_error() {
    assert!(
        Command::parse(&[
            "unidentified",
            "--claim",
            "--title",
            "The Matrix",
            "--kind",
            "movie"
        ])
        .is_err()
    );
}

#[test]
fn jobs_parses_as_list() {
    assert_eq!(Command::parse(&["jobs"]).unwrap(), Command::Jobs);
}

#[test]
fn jobs_extra_arg_is_error() {
    assert!(Command::parse(&["jobs", "extra"]).is_err());
}

#[test]
fn jobs_tick_now_parses() {
    assert_eq!(
        Command::parse(&["jobs", "tick", "--now", "1"]).unwrap(),
        Command::JobsTick { now: Some(1) }
    );
}

#[test]
fn search_without_query_is_error() {
    assert!(Command::parse(&["search"]).is_err());
}

#[test]
fn catalog_without_query_is_error() {
    assert!(Command::parse(&["catalog", "--query"]).is_err());
}

#[test]
fn catalog_parses_as_list() {
    assert_eq!(Command::parse(&["catalog"]).unwrap(), Command::Catalog);
}

#[test]
fn catalog_delete_without_key_is_error() {
    assert!(Command::parse(&["catalog", "--delete", "--source", "tmdb"]).is_err());
}

#[test]
fn search_admit_without_subscribe_is_error() {
    assert!(
        Command::parse(&[
            "search",
            "--admit",
            "--enclosure",
            "https://pt.example/download.php?id=1"
        ])
        .is_err()
    );
}

#[tokio::test]
async fn cli_sites_prints_created_site_name() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    create_site(&t.app).await;
    let listed = t.send("GET", "/api/v1/sites", None).await.unwrap();
    let id = listed.1[0]["id"].as_str().unwrap();
    let out = run(Command::Sites, &t).await.unwrap();
    assert!(out.contains("demo"), "{out}");
    assert!(out.contains("https://pt.example/"), "{out}");
    assert!(out.contains(id), "{out}");
    assert!(out.contains("enabled=true"), "{out}");
    assert!(!out.contains("uid=1"), "{out}");
}

#[tokio::test]
async fn cli_subscribe_prints_subscribe_id() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let created = t
        .app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/rule-sets",
            Some("management-secret"),
            json!({
                "name": "uhd",
                "atoms": [{ "kind": "resolution", "value": "2160p", "priority": 100 }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let filter = json_body(created).await;
    t.app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/rule-sets/default",
            Some("management-secret"),
            json!({ "id": filter["data"]["id"] }),
        ))
        .await
        .unwrap();
    let out = run(
        Command::Subscribe {
            title: "The Matrix".into(),
            kind: "movie".into(),
        },
        &t,
    )
    .await
    .unwrap();
    assert!(out.contains("id="), "{out}");
}

#[tokio::test]
async fn cli_subscribe_list_prints_no_subscribes_when_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(Command::Subscribes, &t).await.unwrap();
    assert_eq!(out, "No Subscribes\n");
}

#[tokio::test]
async fn cli_subscribe_list_prints_media_title() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let created = t
        .app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/rule-sets",
            Some("management-secret"),
            json!({
                "name": "uhd",
                "atoms": [{ "kind": "resolution", "value": "2160p", "priority": 100 }]
            }),
        ))
        .await
        .unwrap();
    let filter = json_body(created).await;
    t.app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/rule-sets/default",
            Some("management-secret"),
            json!({ "id": filter["data"]["id"] }),
        ))
        .await
        .unwrap();
    run(
        Command::Subscribe {
            title: "The Matrix".into(),
            kind: "movie".into(),
        },
        &t,
    )
    .await
    .unwrap();
    let out = run(Command::Subscribes, &t).await.unwrap();
    assert!(out.contains("The Matrix"), "{out}");
    assert!(out.contains("movie"), "{out}");
    assert!(out.contains("search"), "{out}");
}

#[tokio::test]
async fn cli_filters_lists_seeded_default_on_fresh_store() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(Command::Filters, &t).await.unwrap();
    assert!(out.contains("默认"), "{out}");
    assert!(out.contains("default"), "{out}");
}

#[tokio::test]
async fn cli_filters_prints_filter_name() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let created = t
        .app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/rule-sets",
            Some("management-secret"),
            json!({
                "name": "uhd",
                "atoms": [{ "kind": "resolution", "value": "2160p", "priority": 100 }]
            }),
        ))
        .await
        .unwrap();
    let filter = json_body(created).await;
    t.app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/rule-sets/default",
            Some("management-secret"),
            json!({ "id": filter["data"]["id"] }),
        ))
        .await
        .unwrap();
    let out = run(Command::Filters, &t).await.unwrap();
    assert!(out.contains("uhd"), "{out}");
    assert!(out.contains("atoms=1"), "{out}");
    assert!(out.contains("default"), "{out}");
}

#[tokio::test]
async fn cli_filters_default_prints_id_and_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    run(
        Command::FiltersAdd {
            name: "uhd".into(),
            atoms: vec![api::cli::FilterAtomSpec {
                kind: "resolution".into(),
                value: Some("2160p".into()),
                priority: 100,
            }],
        },
        &t,
    )
    .await
    .unwrap();
    let listed = t.send("GET", "/api/v1/rule-sets", None).await.unwrap();
    let id = listed.1[0]["id"].as_str().unwrap().to_string();
    let out = run(Command::FiltersDefault { id: id.clone() }, &t)
        .await
        .unwrap();
    assert!(out.contains(&format!("default_filter_id={id}")), "{out}");
    let listed = run(Command::Filters, &t).await.unwrap();
    assert!(listed.contains("default"), "{listed}");
}

#[tokio::test]
async fn cli_filters_add_prints_name_and_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(
        Command::FiltersAdd {
            name: "uhd".into(),
            atoms: vec![api::cli::FilterAtomSpec {
                kind: "resolution".into(),
                value: Some("2160p".into()),
                priority: 100,
            }],
        },
        &t,
    )
    .await
    .unwrap();
    assert!(out.contains("uhd"), "{out}");
    assert!(out.contains("atoms=1"), "{out}");
    let listed = run(Command::Filters, &t).await.unwrap();
    assert!(listed.contains("uhd"), "{listed}");
    assert!(listed.contains("atoms=1"), "{listed}");
}

#[tokio::test]
async fn cli_users_prints_seeded_admin() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(Command::Users, &t).await.unwrap();
    assert!(out.contains("admin"), "{out}");
}

#[tokio::test]
async fn cli_users_prints_second_login_without_token() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    t.app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/users",
            Some("management-secret"),
            json!({ "login": "alice", "password": "alice-token" }),
        ))
        .await
        .unwrap();
    let out = run(Command::Users, &t).await.unwrap();
    assert!(out.contains("admin"), "{out}");
    assert!(out.contains("alice"), "{out}");
    assert!(!out.contains("alice-token"), "{out}");
    assert!(!out.contains("management-secret"), "{out}");
}

#[tokio::test]
async fn cli_users_add_prints_login_and_omits_token() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(
        Command::UsersAdd {
            login: "alice".into(),
            token: "alice-token".into(),
        },
        &t,
    )
    .await
    .unwrap();
    assert!(out.contains("login=alice"), "{out}");
    assert!(!out.contains("alice-token"), "{out}");
    let listed = run(Command::Users, &t).await.unwrap();
    assert!(listed.contains("alice"), "{listed}");
    assert!(!listed.contains("alice-token"), "{listed}");
}
#[tokio::test]
async fn cli_jobs_tick_prints_ran_and_kinds() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(Command::JobsTick { now: Some(1) }, &t).await.unwrap();
    assert!(out.contains("ran="), "{out}");
    assert!(
        out.contains("subscribe_rss")
            || out.contains("transfer")
            || out.contains("watch_intake")
            || out.contains("scrape"),
        "{out}"
    );
}

#[tokio::test]
async fn cli_search_prints_torrent_title() {
    let tmp = tempfile::tempdir().unwrap();
    let t = search_transport(&tmp, HashMap::from([("search", nexusphp())]));
    create_site(&t.app).await;
    let out = run(
        Command::Search {
            query: "matrix".into(),
        },
        &t,
    )
    .await
    .unwrap();
    assert!(out.contains("The.Matrix.1999"), "{out}");
    assert!(out.contains("2160p"), "{out}");
}

#[tokio::test]
async fn cli_search_prints_site_failure_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    create_site(&t.app).await;
    let out = run(
        Command::Search {
            query: "matrix".into(),
        },
        &t,
    )
    .await
    .unwrap();
    assert!(out.contains("failed"), "{out}");
}

#[tokio::test]
async fn cli_admit_prints_torrent_title() {
    let tmp = tempfile::tempdir().unwrap();
    let t = search_transport(&tmp, HashMap::from([("search", nexusphp())]));
    create_site(&t.app).await;
    let subscribe = create_subscribe(&t.app, "search").await;
    let searched = t
        .send("GET", "/api/v1/search/torrents?keyword=matrix", None)
        .await
        .unwrap();
    assert_eq!(searched.0, 200);
    let enclosure = searched.1["items"][0]["enclosure"]
        .as_str()
        .unwrap()
        .to_string();
    let out = run(
        Command::Admit {
            subscribe_id: subscribe["id"].as_str().unwrap().into(),
            enclosure,
        },
        &t,
    )
    .await
    .unwrap();
    assert!(out.contains("The.Matrix.1999"), "{out}");
}

#[tokio::test]
async fn cli_downloads_prints_no_downloads_when_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(Command::Downloads, &t).await.unwrap();
    assert!(out.contains("No Downloads"), "{out}");
}

#[tokio::test]
async fn cli_downloads_prints_pending_torrent_title() {
    let tmp = tempfile::tempdir().unwrap();
    let t = search_transport(&tmp, HashMap::from([("search", nexusphp())]));
    create_site(&t.app).await;
    let subscribe = create_subscribe(&t.app, "search").await;
    let searched = t
        .send("GET", "/api/v1/search/torrents?keyword=matrix", None)
        .await
        .unwrap();
    let enclosure = searched.1["items"][0]["enclosure"]
        .as_str()
        .unwrap()
        .to_string();
    run(
        Command::Admit {
            subscribe_id: subscribe["id"].as_str().unwrap().into(),
            enclosure,
        },
        &t,
    )
    .await
    .unwrap();
    let out = run(Command::Downloads, &t).await.unwrap();
    assert!(out.contains("The.Matrix.1999"), "{out}");
    assert!(out.contains("The Matrix"), "{out}");
}

#[tokio::test]
async fn cli_jobs_prints_transfer_kind() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(Command::Jobs, &t).await.unwrap();
    assert!(out.contains("transfer"), "{out}");
    assert!(out.contains("subscribe_rss"), "{out}");
}

#[tokio::test]
async fn cli_sites_add_prints_name_and_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let _out = run(
        Command::SitesAdd {
            name: "demo".into(),
            url: "https://pt.example/".into(),
            profile_id: "demo".into(),
            cookie: Some("uid=1; pass=abc".into()),
            api_key: None,
        },
        &t,
    )
    .await
    .unwrap();
    let app = router(
        state(
            tmp.path(),
            Arc::new(Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )
        .with_catalog(Arc::new(super::catalog::test_catalog())),
    );
    let t = RouterTransport {
        app,
        token: "management-secret".into(),
    };
    let out = run(
        Command::CatalogSearch {
            query: "matrix".into(),
        },
        &t,
    )
    .await
    .unwrap();
    assert!(out.contains("The Matrix"), "{out}");
    assert!(out.contains("movie"), "{out}");
    assert!(out.contains("tmdb=603"), "{out}");
    assert!(out.contains("douban=-"), "{out}");
    assert!(out.contains("tvdb=-"), "{out}");
    assert!(out.contains("bangumi=-"), "{out}");
    assert!(out.contains("anilist=-"), "{out}");
}

#[tokio::test]
async fn cli_catalog_lists_and_deletes_cache_row() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .put_catalog_cache(
            "tmdb",
            "/search/movie?query=matrix",
            r#"{"results":[{"title":"The Matrix"}]}"#,
            100,
            Some(200),
        )
        .unwrap();
    drop(store);
    let t = transport(&tmp);
    let listed = run(Command::Catalog, &t).await.unwrap();
    assert!(listed.contains("The Matrix"), "{listed}");
    assert!(listed.contains("tmdb"), "{listed}");
    let out = run(
        Command::CatalogDelete {
            source: "tmdb".into(),
            cache_key: "/search/movie?query=matrix".into(),
        },
        &t,
    )
    .await
    .unwrap();
    assert!(out.contains("deleted=true"), "{out}");
    let listed = run(Command::Catalog, &t).await.unwrap();
    assert_eq!(listed, "No cache\n");
}
