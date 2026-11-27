#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::DownloaderId;
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

async fn make_member_env() -> (HttpEnv, Arc<MemoryDownloader>, DownloaderId, DownloaderId) {
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
            Some(json!({ "login": "r9-member", "password": "r9-member-password-123" })),
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
            Some(json!({ "username": "r9-member", "password": "r9-member-password-123" })),
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
    let dl_a = env.insert_downloader("DL-R9-Primary");
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
    let dl_b = env.insert_downloader("DL-R9-Unreachable");
    (env, dl_mem, dl_a, dl_b)
}

fn seed_owned(
    store: &Store,
    op_id: &str,
    dl_id: DownloaderId,
    site_id: domain::SiteId,
    enclosure: &str,
) {
    let tag = format!("cm-op:{op_id}");
    store
        .accept_download_operation(&AcceptDownloadOperationInput {
            operation_id: op_id.into(),
            idempotency_key: format!("ik-{op_id}"),
            source_kind: "plugin".into(),
            subscribe_id: None,
            plugin_id: Some("builtin.boost".into()),
            subject: "system".into(),
            cause_id: format!("c-{op_id}"),
            site_id: Some(site_id),
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            enclosure: enclosure.into(),
            ownership_tag: tag.clone(),
            info_hash: None,
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
            info_hash: None,
            bytes_digest: None,
            operation_id: op_id.into(),
            subscribe_id: None,
            plugin_id: Some("builtin.boost".into()),
            subject: "system".into(),
            site_id: Some(site_id),
            enclosure: enclosure.into(),
            ownership_state: DownloadOwnershipState::Owned,
            provenance_kind: DownloadProvenanceKind::CreatedFresh,
            verified_bytes: Some(4096),
            now_utc: 100,
        })
        .unwrap();
}

#[tokio::test]
async fn member_tasks_redact_all_pt_enclosures_and_match_real_cm_op_snapshots() {
    let (env, dl_mem, dl_a, dl_b) = make_member_env().await;
    let site_id = env.insert_site("PT-R9");
    let live_hash = "cccc3333cccc3333cccc3333cccc3333cccc3333";

    // Submit via real `submit_verified` (populates `owned_tags` via `owned_tags_from`)
    dl_mem
        .submit_verified(
            &SubmitPayload::Magnet {
                uri: format!("magnet:?xt=urn:btih:{live_hash}&dn=Real.Submit.Task.S01E01"),
                info_hash: live_hash.into(),
            },
            &SubmitOptions {
                ownership_tag: "cm-op:op-r9-live".into(),
                ..Default::default()
            },
        )
        .unwrap();
    seed_owned(
        env.state.store().as_ref(),
        "op-r9-live",
        dl_a,
        site_id,
        "https://secret-pt.internal/dl/88.UNIT3D_KEY?sign=MTEAM_SIGN_1",
    );
    seed_owned(
        env.state.store().as_ref(),
        "op-r9-offline",
        dl_b,
        site_id,
        "https://secret-pt.internal/torrent/download/99.UNIT3D_RSS_SECRET?sign=MTEAM_SIGN_SECRET&downhash=NEXUS_SECRET",
    );

    let (st_t, _, b_t) = env
        .send(req_json(
            "GET",
            "/api/v1/downloaders/tasks",
            Some(&env.member_token),
            &[],
            None,
        ))
        .await;
    let tasks_data = assert_ok(st_t, &b_t, StatusCode::OK);
    let tasks_str = tasks_data.to_string();
    for secret in [
        "UNIT3D_RSS_SECRET",
        "MTEAM_SIGN_SECRET",
        "NEXUS_SECRET",
        "secret-pt.internal",
    ] {
        assert!(
            !tasks_str.contains(secret),
            "member /downloaders/tasks leaked `{secret}`: {tasks_str}"
        );
    }
    let items = tasks_data["items"].as_array().unwrap();
    let live_row = items
        .iter()
        .find(|it| it["ownership_tag"] == "cm-op:op-r9-live")
        .expect("live cm-op row");
    assert_neq_missing(live_row);
}

#[tokio::test]
async fn member_activities_redact_deferred_candidate_pt_enclosures() {
    let (env, _dl_mem, _dl_a, _dl_b) = make_member_env().await;
    let (st_rs, _, b_rs) = env
        .send(req_json(
            "POST",
            "/api/v1/rule-sets",
            Some(TOKEN),
            &[],
            Some(json!({
                "name": "R9RuleSet",
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
                    "title": "Show R9",
                    "original_title": "Show R9",
                    "year": 2026
                },
                "coverage": { "kind": "movie" },
                "fetch_mode": "search",
                "filter_id": filter_id
            })),
        ))
        .await;
    let sub_id: domain::SubscribeId = assert_ok(st_sub, &b_sub, StatusCode::CREATED)["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let bind = env
        .state
        .store()
        .subscribe_policy_binding(sub_id)
        .unwrap()
        .unwrap();
    env.state
        .store()
        .record_subscribe_defer(&RecordSubscribeDeferInput {
            defer_id: "def-r9-sec".into(),
            subscribe_id: sub_id,
            subscribe_generation: bind.jobs_sync_generation,
            policy_revision: bind.policy_revision,
            enclosure: "https://secret-pt.internal/torrent/download/77.PATH_PASSKEY?sign=SIGN_TOKEN&torrent_pass=GAZELLE_PASS".into(),
            info_hash: None,
            plugin_id: Some("org.example.p".into()),
            hook_id: "subscribe.select".into(),
            reason: "hold".into(),
            next_retry_at: 2_000,
            now_utc: 1_000,
        })
        .unwrap();

    let (st_a, _, b_a) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/subscriptions/{sub_id}/activities"),
            Some(&env.member_token),
            &[],
            None,
        ))
        .await;
    let acts_str = assert_ok(st_a, &b_a, StatusCode::OK).to_string();
    for secret in [
        "PATH_PASSKEY",
        "SIGN_TOKEN",
        "GAZELLE_PASS",
        "secret-pt.internal",
    ] {
        assert!(
            !acts_str.contains(secret),
            "member /activities leaked `{secret}`: {acts_str}"
        );
    }
}

fn assert_neq_missing(live_row: &serde_json::Value) {
    assert_ne!(
        live_row["state"], "missing",
        "cm-op task submitted via submit_verified must match live snapshot: {live_row}"
    );
}
