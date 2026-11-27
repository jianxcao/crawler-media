#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::{DownloaderId, SubscribeId, Torrent};
use downloader::{Downloader, MemoryDownloader, TaskSnapshot};
use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
use plugin_http_support::{ADMIN_PASS, HttpEnv, TOKEN, assert_ok, req_json};
use serde_json::json;
use store::download_operations::{
    AcceptDownloadOperationInput, DownloadOwnershipState, DownloadProvenanceKind,
    RecordDownloadOwnershipInput,
};
use tower::ServiceExt;

struct OfflineFetcher;
impl Fetcher for OfflineFetcher {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}

async fn make_member_env_with_dl() -> (HttpEnv, Arc<MemoryDownloader>, DownloaderId, DownloaderId) {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl_mem = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(OfflineFetcher),
        dl_mem.clone() as Arc<dyn Downloader>,
    )
    .unwrap();
    let app = router(state.clone());
    let (_, _, create_body) = {
        let req = req_json(
            "POST",
            "/api/v1/users",
            Some(TOKEN),
            &[],
            Some(json!({ "login": "r8-member", "password": "r8-member-password-123" })),
        );
        let resp = app.clone().oneshot(req).await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            StatusCode::CREATED,
            (),
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
        )
    };
    let _ = create_body;
    let login_resp = app
        .clone()
        .oneshot(req_json(
            "POST",
            "/api/v1/auth/login",
            None,
            &[],
            Some(json!({ "username": "r8-member", "password": "r8-member-password-123" })),
        ))
        .await
        .unwrap();
    let login_bytes = axum::body::to_bytes(login_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let login_json: serde_json::Value = serde_json::from_slice(&login_bytes).unwrap();
    let member_token = login_json["data"]["token"].as_str().unwrap().to_string();

    let env = HttpEnv {
        _dir: dir,
        state,
        app,
        member_token,
    };
    let dl_a = env.insert_downloader("DL-Primary");
    env.state
        .store()
        .with_writer(store::Db::App, |c| {
            c.execute(
                "UPDATE downloaders SET is_default = 1 WHERE id = ?1",
                [dl_a.to_string()],
            )?;
            Ok(())
        })
        .unwrap();
    let dl_b = env.insert_downloader("DL-Secondary");
    (env, dl_mem, dl_a, dl_b)
}

fn seed_owned_task(
    store: &Store,
    op_id: &str,
    dl_id: DownloaderId,
    site_id: domain::SiteId,
    info_hash: Option<&str>,
    enclosure: &str,
) {
    let tag = format!("cm-op:{op_id}");
    store
        .accept_download_operation(&AcceptDownloadOperationInput {
            operation_id: op_id.into(),
            idempotency_key: format!("ik-{op_id}"),
            source_kind: "plugin".into(),
            subscribe_id: None,
            plugin_id: Some("cm.boost".into()),
            subject: "system".into(),
            cause_id: format!("c-{op_id}"),
            site_id: Some(site_id),
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            enclosure: enclosure.into(),
            ownership_tag: tag.clone(),
            info_hash: info_hash.map(str::to_string),
            bytes_digest: None,
            verified_bytes: Some(4096),
            request_digest: format!("rd-{op_id}"),
            now_utc: 100,
        })
        .unwrap();
    store
        .record_download_ownership(&RecordDownloadOwnershipInput {
            downloader_id: dl_id,
            ownership_tag: tag,
            info_hash: info_hash.map(str::to_string),
            operation_id: op_id.into(),
            subscribe_id: None,
            plugin_id: Some("cm.boost".into()),
            subject: "system".into(),
            site_id: Some(site_id),
            enclosure: enclosure.into(),
            ownership_state: DownloadOwnershipState::Owned,
            provenance_kind: DownloadProvenanceKind::CreatedFresh,
            verified_bytes: Some(4096),
            bytes_digest: None,
            now_utc: 100,
        })
        .unwrap();
}

async fn seed_subscribe_pending(
    env: &HttpEnv,
    site_id: domain::SiteId,
    dl_id: DownloaderId,
    info_hash: &str,
) {
    let (st_rs, _, b_rs) = env
        .send(req_json(
            "POST",
            "/api/v1/rule-sets",
            Some(TOKEN),
            &[],
            Some(json!({
                "name": "R8RuleSet",
                "atoms": [{ "kind": "title_match", "value": "Show", "priority": 80 }]
            })),
        ))
        .await;
    let filter_id = assert_ok(st_rs, &b_rs, StatusCode::CREATED)["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (st_sub, _, b_sub) = env
        .send(req_json(
            "POST",
            "/api/v1/subscriptions",
            Some(TOKEN),
            &[],
            Some(json!({
                "media": {
                    "kind": "movie",
                    "title": "Show R8",
                    "original_title": "Show R8",
                    "year": 2026
                },
                "coverage": { "kind": "movie" },
                "fetch_mode": "search",
                "filter_id": filter_id
            })),
        ))
        .await;
    let sub_id: SubscribeId = assert_ok(st_sub, &b_sub, StatusCode::CREATED)["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let sub_torrent = Torrent {
        site_id,
        id: Some("10".into()),
        title: "Show.R8.S01E01.1080p".into(),
        enclosure: "https://pt.example/sub.torrent".into(),
        size_bytes: Some(2048),
        seeders: Some(5),
        leechers: None,
        snatched: None,
        upload_time: None,
        poster_url: None,
        free: true,
        hr: Some(false),
        imdb_id: None,
        detail_url: None,
        category: None,
        free_2x: false,
        free_until: None,
    };
    env.state
        .store()
        .record_pending_submission(
            sub_id,
            90,
            &store::PendingDownload {
                torrent: sub_torrent.clone(),
                downloader_id: Some(dl_id),
                submitted_at: Some(api::job_loop::unix_now()),
                release_override: None,
            },
        )
        .unwrap();
    env.state
        .store()
        .set_pending_info_hash(sub_id, &sub_torrent.enclosure, info_hash)
        .unwrap();
}

#[tokio::test]
async fn member_tasks_endpoint_redacts_enclosure_secrets_matches_ownership_tag_and_keeps_cross_downloader_tasks()
 {
    let (env, dl_mem, dl_a, dl_b) = make_member_env_with_dl().await;
    let site_id = env.insert_site("PT-SliceC-R8");
    let shared_hash = "aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111";
    let tagonly_hash = "bbbb2222bbbb2222bbbb2222bbbb2222bbbb2222";

    dl_mem.push_snapshot(TaskSnapshot {
        tag: String::new(),
        name: "Sub.Show.S01E01.1080p".into(),
        progress: 1.0,
        state: "seeding".into(),
        size_bytes: 2048,
        downloaded_bytes: 2048,
        uploaded_bytes: 0,
        download_speed: 0,
        upload_speed: 0,
        info_hash: shared_hash.into(),
        owned_tags: vec![downloader::ownership_tag("https://pt.example/sub.torrent")],
        ..Default::default()
    });
    dl_mem.push_snapshot(TaskSnapshot {
        tag: String::new(),
        name: "TagOnly.Live.Task.mkv".into(),
        progress: 1.0,
        state: "seeding".into(),
        size_bytes: 4096,
        downloaded_bytes: 4096,
        uploaded_bytes: 512,
        download_speed: 0,
        upload_speed: 0,
        info_hash: tagonly_hash.into(),
        owned_tags: vec!["cm-op:op-r8-tagonly".into()],
        ..Default::default()
    });
    seed_subscribe_pending(&env, site_id, dl_a, shared_hash).await;

    // 1) Owned task on secondary downloader `dl_b` (unreachable) with SAME `shared_hash` & secret passkey URL.
    seed_owned_task(
        env.state.store().as_ref(),
        "op-r8-cross-dl",
        dl_b,
        site_id,
        Some(shared_hash),
        "https://user:SECRET_PW@pt.example/download.php?id=99&passkey=SUPER_SECRET_PK",
    );
    // 2) Owned task on primary downloader `dl_a` with `info_hash: None`, live in `dl_mem` via `ownership_tag`!
    seed_owned_task(
        env.state.store().as_ref(),
        "op-r8-tagonly",
        dl_a,
        site_id,
        None,
        "https://pt.example/download.php?id=88&api_key=SECRET_API_KEY",
    );

    let (st, _, body) = env
        .send(req_json(
            "GET",
            "/api/v1/downloaders/tasks",
            Some(&env.member_token),
            &[],
            None,
        ))
        .await;
    let data = assert_ok(st, &body, StatusCode::OK);
    let dumped = data.to_string();
    assert!(
        !dumped.contains("SUPER_SECRET_PK")
            && !dumped.contains("SECRET_PW")
            && !dumped.contains("SECRET_API_KEY"),
        "member /downloaders/tasks must NEVER leak raw PT enclosure credentials: {dumped}"
    );
    let items = data["items"].as_array().unwrap();
    assert!(
        items
            .iter()
            .any(|it| it["ownership_tag"] == "cm-op:op-r8-cross-dl"),
        "owned task on secondary downloader must NOT be dropped due to hash collision: {dumped}"
    );
    let tagonly = items
        .iter()
        .find(|it| it["ownership_tag"] == "cm-op:op-r8-tagonly")
        .expect("tag-only owned task must be present");
    assert_eq!(tagonly["state"], "completed", "tagonly={tagonly}");
    assert_eq!(tagonly["info_hash"], tagonly_hash);
    assert_eq!(tagonly["name"], "TagOnly.Live.Task.mkv");
}
