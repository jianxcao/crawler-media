#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::{DownloaderId, Torrent};
use downloader::{Downloader, MemoryDownloader, SubmitOptions, SubmitPayload};
use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
use plugin_http_support::{ADMIN_PASS, HttpEnv, TOKEN, assert_ok, req_json};
use serde_json::json;
use store::download_operations::{
    AcceptDownloadOperationInput, DownloadOwnershipState, DownloadProvenanceKind,
    RecordDownloadOwnershipInput, RecordSubscribeDeferInput,
};
use tower::ServiceExt;

struct OfflineFetcher;
impl Fetcher for OfflineFetcher {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}

async fn make_r10_env() -> (HttpEnv, Arc<MemoryDownloader>, DownloaderId) {
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
    let _ = app
        .clone()
        .oneshot(req_json(
            "POST",
            "/api/v1/users",
            Some(TOKEN),
            &[],
            Some(json!({ "login": "r10-member", "password": "r10-member-password-123" })),
        ))
        .await
        .unwrap();
    let login_resp = app
        .clone()
        .oneshot(req_json(
            "POST",
            "/api/v1/auth/login",
            None,
            &[],
            Some(json!({ "username": "r10-member", "password": "r10-member-password-123" })),
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
    let dl_id = env.insert_downloader("DL-R10-Default");
    env.state
        .store()
        .with_writer(store::Db::App, |c| {
            c.execute(
                "UPDATE downloaders SET is_default = 1 WHERE id = ?1",
                [dl_id.to_string()],
            )?;
            Ok(())
        })
        .unwrap();
    (env, dl_mem, dl_id)
}

async fn create_member_sub(env: &HttpEnv, title: &str) -> domain::SubscribeId {
    let (st_rs, _, b_rs) = env
        .send(req_json(
            "POST",
            "/api/v1/rule-sets",
            Some(TOKEN),
            &[],
            Some(json!({
                "name": format!("RS-{title}"),
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
            Some(&env.member_token),
            &[],
            Some(json!({
                "media": {
                    "kind": "movie",
                    "title": title,
                    "original_title": title,
                    "year": 2026
                },
                "coverage": { "kind": "movie" },
                "fetch_mode": "search",
                "filter_id": filter_id
            })),
        ))
        .await;
    assert_ok(st_sub, &b_sub, StatusCode::CREATED)["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

fn seed_r10_owned_op(
    store: &Store,
    sub_id: domain::SubscribeId,
    dl_id: DownloaderId,
    site_id: domain::SiteId,
    enc: &str,
    hash: &str,
) {
    store
        .accept_download_operation(&AcceptDownloadOperationInput {
            operation_id: "op-r10-sub".into(),
            idempotency_key: "ik-op-r10-sub".into(),
            source_kind: "subscribe".into(),
            subscribe_id: Some(sub_id),
            plugin_id: None,
            subject: "system".into(),
            cause_id: "c-r10".into(),
            site_id: Some(site_id),
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            enclosure: enc.into(),
            ownership_tag: "cm-op:op-r10-sub".into(),
            info_hash: Some(hash.into()),
            bytes_digest: None,
            verified_bytes: Some(8192),
            request_digest: "rd-r10-sub".into(),
            now_utc: 100,
        })
        .unwrap();
    store
        .record_download_ownership(&RecordDownloadOwnershipInput {
            downloader_id: dl_id,
            ownership_tag: "cm-op:op-r10-sub".into(),
            info_hash: Some(hash.into()),
            bytes_digest: None,
            operation_id: "op-r10-sub".into(),
            subscribe_id: Some(sub_id),
            plugin_id: None,
            subject: "system".into(),
            site_id: Some(site_id),
            enclosure: enc.into(),
            ownership_state: DownloadOwnershipState::Owned,
            provenance_kind: DownloadProvenanceKind::CreatedFresh,
            verified_bytes: Some(8192),
            now_utc: 100,
        })
        .unwrap();
}

#[tokio::test]
async fn http_subscription_task_deduplicates_with_ownership_and_turns_missing_after_delete() {
    let (env, dl_mem, dl_id) = make_r10_env().await;
    let sub_id = create_member_sub(&env, "Show R10 Tasks").await;
    let site_id = env.insert_site("PT-R10-Tasks");
    let hash = "dddd4444dddd4444dddd4444dddd4444dddd4444";
    let enc = "https://secret-pt.internal/torrent/download/77.PATH_PASSKEY?sign=SIGN_TOKEN";
    let torrent = Torrent {
        site_id,
        title: "Show.R10.Tasks.2026.1080p.mkv".into(),
        enclosure: enc.into(),
        size_bytes: Some(8192),
        seeders: Some(10),
        free: false,
        hr: Some(false),
        imdb_id: None,
        id: None,
        leechers: Some(0),
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    };

    dl_mem
        .submit_verified(
            &SubmitPayload::Magnet {
                uri: format!("magnet:?xt=urn:btih:{hash}&dn=Show.R10.Tasks"),
                info_hash: hash.into(),
            },
            &SubmitOptions {
                ownership_tag: "cm-op:op-r10-sub".into(),
                paused: true,
                ..Default::default()
            },
        )
        .unwrap();
    dl_mem.remember_identity(&torrent, hash).unwrap();
    env.state
        .store()
        .record_pending_submission(
            sub_id,
            80,
            &store::PendingDownload {
                torrent: torrent.clone(),
                release_override: None,
                downloader_id: Some(dl_id),
                submitted_at: Some(100),
            },
        )
        .unwrap();
    env.state
        .store()
        .set_pending_info_hash(sub_id, enc, hash)
        .unwrap();
    seed_r10_owned_op(
        env.state.store().as_ref(),
        sub_id,
        dl_id,
        site_id,
        enc,
        hash,
    );

    let (st_t, _, b_t) = env
        .send(req_json(
            "GET",
            "/api/v1/downloaders/tasks",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let items = assert_ok(st_t, &b_t, StatusCode::OK)["items"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(
        items.len(),
        1,
        "must deduplicate pending + ownership into 1 row: {items:?}"
    );
    assert_eq!(items[0]["info_hash"], hash);
    assert_eq!(items[0]["state"], "paused");

    dl_mem.delete_task(hash, false).unwrap();
    let (st_t2, _, b_t2) = env
        .send(req_json(
            "GET",
            "/api/v1/downloaders/tasks",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let items2 = assert_ok(st_t2, &b_t2, StatusCode::OK)["items"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(items2[0]["state"], "missing");
    assert_eq!(items2[0]["info_hash"], hash);
}

#[tokio::test]
async fn admin_and_member_activities_and_jobs_redact_pt_path_passkeys_and_member_urls() {
    let (env, _dl_mem, _dl_id) = make_r10_env().await;
    let sub_id = create_member_sub(&env, "Show R10 Redact").await;
    let bind = env
        .state
        .store()
        .subscribe_policy_binding(sub_id)
        .unwrap()
        .unwrap();
    env.state
        .store()
        .record_subscribe_defer(&RecordSubscribeDeferInput {
            defer_id: "def-r10-admin".into(),
            subscribe_id: sub_id,
            subscribe_generation: bind.jobs_sync_generation,
            policy_revision: bind.policy_revision,
            enclosure: "https://secret-pt.internal/torrent/download/77.PATH_PASSKEY?sign=SIGN_TOKEN".into(),
            info_hash: None,
            plugin_id: Some("org.example.p".into()),
            hook_id: "subscribe.select".into(),
            reason: "upstream failed https://secret-pt.internal/dl/88.UNIT3D_KEY".into(),
            next_retry_at: 2_000,
            now_utc: 1_000,
        })
        .unwrap();

    // Seed a failed subscribe_search job carrying a PT URL error (both standalone and under def)
    let job_err = "http fetch returned non-success status 404 for https://secret-pt.internal/torrent/download/77.PATH_PASSKEY?passkey=QUERY_SECRET";
    {
        let q = env.state.jobs.acquire().unwrap();
        let defs = q.list_defs().unwrap();
        let sub_def = defs
            .iter()
            .find(|d| d.scope == jobs::JobScope::Subscribe(sub_id))
            .expect("subscribe_search def");
        let _ = q.ensure_scheduled_for(1, sub_def.id).unwrap();
        for claimed in q.claim(i64::MAX / 2, 10).unwrap() {
            let _ = q.fail(claimed.id, 11, job_err).unwrap();
        }
    }

    let (st_adm, _, b_adm) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/subscriptions/{sub_id}/activities"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let adm_str = assert_ok(st_adm, &b_adm, StatusCode::OK).to_string();
    for secret in ["PATH_PASSKEY", "SIGN_TOKEN", "UNIT3D_KEY", "QUERY_SECRET"] {
        assert!(
            !adm_str.contains(secret),
            "admin /activities leaked `{secret}`: {adm_str}"
        );
    }

    let (st_mem_act, _, b_mem_act) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/subscriptions/{sub_id}/activities"),
            Some(&env.member_token),
            &[],
            None,
        ))
        .await;
    let mem_act_str = assert_ok(st_mem_act, &b_mem_act, StatusCode::OK).to_string();
    for secret in [
        "PATH_PASSKEY",
        "UNIT3D_KEY",
        "QUERY_SECRET",
        "secret-pt.internal",
    ] {
        assert!(
            !mem_act_str.contains(secret),
            "member /activities leaked `{secret}`: {mem_act_str}"
        );
    }

    let (st_jobs, _, b_jobs) = env
        .send(req_json(
            "GET",
            "/api/v1/jobs",
            Some(&env.member_token),
            &[],
            None,
        ))
        .await;
    let mem_jobs_str = assert_ok(st_jobs, &b_jobs, StatusCode::OK).to_string();
    for secret in ["PATH_PASSKEY", "QUERY_SECRET", "secret-pt.internal"] {
        assert!(
            !mem_jobs_str.contains(secret),
            "member /jobs leaked `{secret}`: {mem_jobs_str}"
        );
    }
}

#[tokio::test]
async fn idempotency_key_replay_rejects_cancelled_or_deleted_library_with_409() {
    let (env, _dl_mem, _dl_id) = make_r10_env().await;
    let extra_lib_dir = env._dir.path().join("extra-movies");
    std::fs::create_dir_all(&extra_lib_dir).unwrap();
    let (st_lib, _, b_lib) = env
        .send(req_json(
            "POST",
            "/api/v1/libraries",
            Some(TOKEN),
            &[],
            Some(json!({
                "name": "Extra Movies",
                "kind": "movie",
                "root_paths": [extra_lib_dir.to_string_lossy()]
            })),
        ))
        .await;
    let extra_lib_id = assert_ok(st_lib, &b_lib, StatusCode::OK)["id"]
        .as_str()
        .unwrap()
        .to_string();

    let sub_id = create_member_sub(&env, "Show R10 Fence").await;
    env.state
        .store()
        .with_writer(store::Db::Subscribe, |c| {
            c.execute(
                "UPDATE subscribes SET library_id = ?1 WHERE id = ?2",
                rusqlite::params![extra_lib_id, sub_id.to_string()],
            )?;
            Ok(())
        })
        .unwrap();

    let hash = "eeee5555eeee5555eeee5555eeee5555eeee5555";
    let magnet = format!("magnet:?xt=urn:btih:{hash}&dn=Show.R10.Fence.2026.1080p");

    let submit_body = json!({
        "subscribe_id": sub_id.to_string(),
        "title": "Show.R10.Fence.2026.1080p",
        "download_url": magnet,
        "size_bytes": 4096,
        "force": true
    });
    let (st1, _, b1) = env
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(&env.member_token),
            &[("Idempotency-Key", "idem-r10-lib-fence")],
            Some(submit_body.clone()),
        ))
        .await;
    let _ = assert_ok(st1, &b1, StatusCode::OK);

    // Clear pending row and start ADR-0012 deletion on target extra movie library
    env.state
        .store()
        .with_writer(store::Db::Subscribe, |c| {
            c.execute(
                "DELETE FROM pending_downloads WHERE subscribe_id = ?1",
                [sub_id.to_string()],
            )?;
            Ok(())
        })
        .unwrap();
    api::library_lifecycle::start_library_removal(&env.state, &extra_lib_id).unwrap();

    let (st2, _, b2) = env
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(&env.member_token),
            &[("Idempotency-Key", "idem-r10-lib-fence")],
            Some(submit_body),
        ))
        .await;
    assert_eq!(
        st2,
        StatusCode::CONFLICT,
        "replay into cancelled library must return 409: {b2}"
    );
    assert_eq!(b2["error"]["code"], "library.unavailable");
    assert!(
        env.state.store().load_pending(sub_id).unwrap().is_empty(),
        "replay into cancelled library must not recreate pending_downloads row"
    );
}
