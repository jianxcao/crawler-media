use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::SubscribeId;
use downloader::{
    ClientSubmitResult, Downloader, DownloaderError, MemoryDownloader, SubmitOptions,
    SubmitPayload, TaskSnapshot,
};
use indexer::RegistrySnapshot;
use serde_json::json;

use super::plugin_http_support::{ADMIN_PASS, HttpEnv, TOKEN, assert_err, assert_ok, req_json};
use super::support::{
    StubSearchFetcher, StubTorrentServer, enable_hooks_plugin, minimal_torrent_bytes, nexus_row,
    seed_site_and_subscribe, setup_subscribe_env,
};

#[tokio::test]
async fn admit_same_idempotency_key_retries_after_failed_500_to_200_clean_pending() {
    let ctx = setup_subscribe_env("The.Matrix.1999.1080p.BluRay.x264", "admit.torrent").await;
    let url = ctx.srv.url("admit.torrent");
    let hdr = [("idempotency-key", "idem-admit-fail-1")];
    let body = json!({ "subscribe_id": ctx.sub_id.to_string(), "enclosure": url });

    *ctx.srv.status_override.lock().unwrap() = 500;
    let (st1, _, b1) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/search/admit",
            Some(TOKEN),
            &hdr,
            Some(body.clone()),
        ))
        .await;
    assert_err(st1, &b1, StatusCode::BAD_REQUEST, "subscribe.invalid");
    assert!(
        ctx.env
            .state
            .store()
            .load_pending(ctx.sub_id)
            .unwrap()
            .is_empty(),
        "failed admit must leave zero pending rows"
    );

    *ctx.srv.status_override.lock().unwrap() = 200;
    let (st2, _, b2) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/search/admit",
            Some(TOKEN),
            &hdr,
            Some(body.clone()),
        ))
        .await;
    assert_ok(st2, &b2, StatusCode::OK);
    let pending = ctx.env.state.store().load_pending(ctx.sub_id).unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].1.submitted_at.is_some());
    assert_eq!(ctx.dl.inner.submitted().len(), 1);

    let (st3, _, b3) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/search/admit",
            Some(TOKEN),
            &hdr,
            Some(body),
        ))
        .await;
    assert_ok(st3, &b3, StatusCode::OK);
    assert_eq!(
        ctx.dl.inner.submitted().len(),
        1,
        "replay after Submitted must not duplicate downloader add"
    );
}

#[tokio::test]
async fn manual_submit_same_idempotency_key_retries_after_failed_500_to_200() {
    let ctx = setup_subscribe_env("The.Matrix.1999.1080p.BluRay.x264", "admit.torrent").await;
    let submit_url = ctx.srv.url("manual.torrent");
    let hdr = [("idempotency-key", "idem-manual-fail-1")];
    let body = json!({
        "download_url": submit_url,
        "title": "The.Matrix.1999.1080p",
        "site_id": ctx.site_id.to_string(),
    });

    *ctx.srv.status_override.lock().unwrap() = 500;
    let (st1, _, b1) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(TOKEN),
            &hdr,
            Some(body.clone()),
        ))
        .await;
    assert_err(st1, &b1, StatusCode::BAD_REQUEST, "downloader.add_failed");

    *ctx.srv.status_override.lock().unwrap() = 200;
    let (st2, _, b2) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(TOKEN),
            &hdr,
            Some(body.clone()),
        ))
        .await;
    assert_ok(st2, &b2, StatusCode::OK);
    assert_eq!(ctx.dl.inner.submitted().len(), 1);

    let (st3, _, b3) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(TOKEN),
            &hdr,
            Some(body),
        ))
        .await;
    assert_ok(st3, &b3, StatusCode::OK);
    assert_eq!(ctx.dl.inner.submitted().len(), 1);

    let mismatch = json!({
        "download_url": ctx.srv.url("other.torrent"),
        "title": "The.Matrix.1999.1080p",
        "site_id": ctx.site_id.to_string(),
    });
    let (st_mis, _, b_mis) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(TOKEN),
            &hdr,
            Some(mismatch),
        ))
        .await;
    assert_err(
        st_mis,
        &b_mis,
        StatusCode::BAD_REQUEST,
        "downloader.add_failed",
    );
}

#[tokio::test]
async fn admit_same_idempotency_key_retries_after_deferred_cleared_clean_pending() {
    let ctx = setup_subscribe_env("The.Matrix.DeferDownload.1080p", "defer.torrent").await;
    let pid = "org.example.admit-idem-defer";
    enable_hooks_plugin(&ctx.env, pid, &["download.before"]).await;
    let url = ctx.srv.url("defer.torrent");
    let hdr = [("idempotency-key", "idem-admit-defer-1")];
    let body = json!({ "subscribe_id": ctx.sub_id.to_string(), "enclosure": url });

    let (st1, _, b1) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/search/admit",
            Some(TOKEN),
            &hdr,
            Some(body.clone()),
        ))
        .await;
    assert_err(st1, &b1, StatusCode::CONFLICT, "plugin.deferred");
    assert!(
        ctx.env
            .state
            .store()
            .load_pending(ctx.sub_id)
            .unwrap()
            .is_empty()
    );

    let (_, sub_other) = seed_site_and_subscribe(&ctx.env, &ctx.srv.url("")).await;
    let (st_mis, _, b_mis) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/search/admit",
            Some(TOKEN),
            &hdr,
            Some(json!({ "subscribe_id": sub_other.to_string(), "enclosure": url })),
        ))
        .await;
    assert_err(st_mis, &b_mis, StatusCode::BAD_REQUEST, "subscribe.invalid");

    let rev = ctx.env.get_plugin_rev(pid).await;
    ctx.env.disable_plugin(pid, rev, "dis-admit-def").await;

    let (st2, _, b2) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/search/admit",
            Some(TOKEN),
            &hdr,
            Some(body.clone()),
        ))
        .await;
    assert_ok(st2, &b2, StatusCode::OK);
    let pending = ctx.env.state.store().load_pending(ctx.sub_id).unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].1.submitted_at.is_some());
    assert_eq!(ctx.dl.inner.submitted().len(), 1);

    let (st3, _, b3) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/search/admit",
            Some(TOKEN),
            &hdr,
            Some(body),
        ))
        .await;
    assert_ok(st3, &b3, StatusCode::OK);
    assert_eq!(ctx.dl.inner.submitted().len(), 1);
}

#[tokio::test]
async fn manual_submit_same_idempotency_key_retries_after_deferred_cleared() {
    let ctx = setup_subscribe_env("The.Matrix.DeferDownload.1080p", "defer.torrent").await;
    let pid = "org.example.manual-idem-defer";
    enable_hooks_plugin(&ctx.env, pid, &["download.before"]).await;
    let hdr = [("idempotency-key", "idem-manual-defer-1")];
    let body = json!({
        "download_url": ctx.srv.url("manual-defer.torrent"),
        "title": "The.Matrix.DeferDownload.1080p",
        "site_id": ctx.site_id.to_string(),
    });

    let (st1, _, b1) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(TOKEN),
            &hdr,
            Some(body.clone()),
        ))
        .await;
    assert_err(st1, &b1, StatusCode::CONFLICT, "plugin.deferred");

    let (st_mis, _, b_mis) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(TOKEN),
            &hdr,
            Some(json!({
                "download_url": ctx.srv.url("other.torrent"),
                "title": "The.Matrix.DeferDownload.1080p",
                "site_id": ctx.site_id.to_string(),
            })),
        ))
        .await;
    assert_err(
        st_mis,
        &b_mis,
        StatusCode::BAD_REQUEST,
        "downloader.add_failed",
    );

    let rev = ctx.env.get_plugin_rev(pid).await;
    ctx.env.disable_plugin(pid, rev, "dis-man-def").await;

    let (st2, _, b2) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(TOKEN),
            &hdr,
            Some(body.clone()),
        ))
        .await;
    assert_ok(st2, &b2, StatusCode::OK);
    assert_eq!(ctx.dl.inner.submitted().len(), 1);

    let (st3, _, b3) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(TOKEN),
            &hdr,
            Some(body),
        ))
        .await;
    assert_ok(st3, &b3, StatusCode::OK);
    assert_eq!(ctx.dl.inner.submitted().len(), 1);
}

struct ReleaseGuard(Option<mpsc::Sender<()>>);
impl Drop for ReleaseGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}

struct BarrierDl {
    inner: MemoryDownloader,
    calls: AtomicUsize,
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

impl Downloader for BarrierDl {
    fn submit_verified(
        &self,
        payload: &SubmitPayload,
        options: &SubmitOptions,
    ) -> Result<ClientSubmitResult, DownloaderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(tx) = self.entered.lock().unwrap().take() {
            let _ = tx.send(());
        }
        if let Some(rx) = self.release.lock().unwrap().take() {
            let _ = rx.recv_timeout(std::time::Duration::from_secs(3));
        }
        self.inner.submit_verified(payload, options)
    }
    fn completed_files(
        &self,
        t: &domain::Torrent,
    ) -> Result<Vec<std::path::PathBuf>, DownloaderError> {
        self.inner.completed_files(t)
    }
    fn task_snapshots(&self) -> Result<Vec<TaskSnapshot>, DownloaderError> {
        self.inner.task_snapshots()
    }
}

fn make_barrier_env(
    payload: &str,
    srv_addr: std::net::SocketAddr,
) -> (
    HttpEnv,
    Arc<BarrierDl>,
    tokio::sync::oneshot::Receiver<()>,
    mpsc::Sender<()>,
) {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let (ent_tx, ent_rx) = tokio::sync::oneshot::channel();
    let (rel_tx, rel_rx) = mpsc::channel();
    let dl = Arc::new(BarrierDl {
        inner: MemoryDownloader::new(dir.path().join("stage")),
        calls: AtomicUsize::new(0),
        entered: Mutex::new(Some(ent_tx)),
        release: Mutex::new(Some(rel_rx)),
    });
    let state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(StubSearchFetcher {
            payload: payload.to_string(),
        }),
        dl.clone(),
    )
    .unwrap()
    .with_plugin_http_policy_override(
        net_guard::TargetPolicy::production().with_test_loopback_socket(srv_addr),
    );
    let env = HttpEnv {
        _dir: dir,
        state: state.clone(),
        app: router(state),
        member_token: String::new(),
    };
    (env, dl, ent_rx, rel_tx)
}

#[tokio::test]
async fn admit_and_manual_submitting_replays_with_zero_duplicate_submit() {
    let srv = StubTorrentServer::start(minimal_torrent_bytes("The.Matrix.1999.1080p", 2048));
    let (env, dl, ent_rx, rel_tx) = make_barrier_env(
        &nexus_row("The.Matrix.1999.1080p", "admit.torrent"),
        srv.addr,
    );
    let (_site_id, sub_id) = seed_site_and_subscribe(&env, &srv.url("")).await;
    let body = json!({ "subscribe_id": sub_id.to_string(), "enclosure": srv.url("admit.torrent") });
    let hdr = [("idempotency-key", "idem-submitting-admit")];
    let env_arc = Arc::new(env);
    let env_bg = Arc::clone(&env_arc);
    let body_bg = body.clone();
    let h1 = tokio::spawn(async move {
        env_bg
            .send(req_json(
                "POST",
                "/api/v1/search/admit",
                Some(TOKEN),
                &[("idempotency-key", "idem-submitting-admit")],
                Some(body_bg),
            ))
            .await
    });
    ent_rx.await.unwrap();

    let rel_guard = ReleaseGuard(Some(rel_tx));
    let (st_rep, _, b_rep) = env_arc
        .send(req_json(
            "POST",
            "/api/v1/search/admit",
            Some(TOKEN),
            &hdr,
            Some(body),
        ))
        .await;
    assert_ok(st_rep, &b_rep, StatusCode::OK);
    assert_eq!(dl.calls.load(Ordering::SeqCst), 1);

    let (_, sub_other): (_, SubscribeId) = seed_site_and_subscribe(&env_arc, &srv.url("")).await;
    let (st_mis, _, b_mis) = env_arc
        .send(req_json(
            "POST",
            "/api/v1/search/admit",
            Some(TOKEN),
            &hdr,
            Some(json!({ "subscribe_id": sub_other.to_string(), "enclosure": srv.url("admit.torrent") })),
        ))
        .await;
    assert_err(st_mis, &b_mis, StatusCode::BAD_REQUEST, "subscribe.invalid");

    drop(rel_guard);
    let (st1, _, b1) = h1.await.unwrap();
    assert_ok(st1, &b1, StatusCode::OK);
    assert_eq!(dl.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn manual_submitting_replays_with_zero_duplicate_submit() {
    let srv = StubTorrentServer::start(minimal_torrent_bytes("The.Matrix.1999.1080p", 2048));
    let (env, dl, ent_rx, rel_tx) = make_barrier_env("", srv.addr);
    let (site_id, _sub_id) = seed_site_and_subscribe(&env, &srv.url("")).await;
    let body = json!({
        "download_url": srv.url("manual-submitting.torrent"),
        "title": "The.Matrix.1999.1080p",
        "site_id": site_id.to_string(),
    });
    let hdr = [("idempotency-key", "idem-submitting-manual")];
    let env_arc = Arc::new(env);
    let env_bg = Arc::clone(&env_arc);
    let body_bg = body.clone();
    let h1 = tokio::spawn(async move {
        env_bg
            .send(req_json(
                "POST",
                "/api/v1/downloaders/submit",
                Some(TOKEN),
                &[("idempotency-key", "idem-submitting-manual")],
                Some(body_bg),
            ))
            .await
    });
    ent_rx.await.unwrap();

    let rel_guard = ReleaseGuard(Some(rel_tx));
    let (st_rep, _, b_rep) = env_arc
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(TOKEN),
            &hdr,
            Some(body),
        ))
        .await;
    assert_ok(st_rep, &b_rep, StatusCode::OK);
    assert_eq!(dl.calls.load(Ordering::SeqCst), 1);

    let (st_mis, _, b_mis) = env_arc
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(TOKEN),
            &hdr,
            Some(json!({
                "download_url": srv.url("other.torrent"),
                "title": "The.Matrix.1999.1080p",
                "site_id": site_id.to_string(),
            })),
        ))
        .await;
    assert_err(
        st_mis,
        &b_mis,
        StatusCode::BAD_REQUEST,
        "downloader.add_failed",
    );

    drop(rel_guard);
    let (st1, _, b1) = h1.await.unwrap();
    assert_ok(st1, &b1, StatusCode::OK);
    assert_eq!(dl.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn admit_and_manual_rejected_replays_and_mismatch_body_returns_conflict() {
    let ctx = setup_subscribe_env("The.Matrix.RejectDownload.1080p", "reject.torrent").await;
    enable_hooks_plugin(&ctx.env, "org.example.reject-idem", &["download.before"]).await;
    let hdr_a = [("idempotency-key", "idem-admit-rej")];
    let body_a = json!({
        "subscribe_id": ctx.sub_id.to_string(),
        "enclosure": ctx.srv.url("reject.torrent"),
    });
    let (sa1, _, ba1) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/search/admit",
            Some(TOKEN),
            &hdr_a,
            Some(body_a.clone()),
        ))
        .await;
    assert_err(sa1, &ba1, StatusCode::FORBIDDEN, "plugin.rejected");
    let (sa2, _, ba2) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/search/admit",
            Some(TOKEN),
            &hdr_a,
            Some(body_a),
        ))
        .await;
    assert_err(sa2, &ba2, StatusCode::FORBIDDEN, "plugin.rejected");
    let (_, sub_other) = seed_site_and_subscribe(&ctx.env, &ctx.srv.url("")).await;
    let (sa_mis, _, ba_mis) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/search/admit",
            Some(TOKEN),
            &hdr_a,
            Some(json!({
                "subscribe_id": sub_other.to_string(),
                "enclosure": ctx.srv.url("reject.torrent"),
            })),
        ))
        .await;
    assert_err(
        sa_mis,
        &ba_mis,
        StatusCode::BAD_REQUEST,
        "subscribe.invalid",
    );
    assert_eq!(ctx.dl.inner.submitted().len(), 0);

    let hdr_m = [("idempotency-key", "idem-manual-rej")];
    let body_m = json!({
        "download_url": ctx.srv.url("reject-man.torrent"),
        "title": "The.Matrix.RejectDownload.1080p",
        "site_id": ctx.site_id.to_string(),
    });
    let (sm1, _, bm1) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(TOKEN),
            &hdr_m,
            Some(body_m.clone()),
        ))
        .await;
    assert_err(sm1, &bm1, StatusCode::BAD_REQUEST, "downloader.add_failed");
    let (sm2, _, bm2) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(TOKEN),
            &hdr_m,
            Some(body_m),
        ))
        .await;
    assert_err(sm2, &bm2, StatusCode::BAD_REQUEST, "downloader.add_failed");
    let (sm_mis, _, bm_mis) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(TOKEN),
            &hdr_m,
            Some(json!({
                "download_url": ctx.srv.url("other-man.torrent"),
                "title": "The.Matrix.RejectDownload.1080p",
                "site_id": ctx.site_id.to_string(),
            })),
        ))
        .await;
    assert_err(
        sm_mis,
        &bm_mis,
        StatusCode::BAD_REQUEST,
        "downloader.add_failed",
    );
    assert_eq!(ctx.dl.inner.submitted().len(), 0);
}
