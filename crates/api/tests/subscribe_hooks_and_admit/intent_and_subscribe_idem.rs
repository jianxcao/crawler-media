use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use indexer::RegistrySnapshot;
use serde_json::json;
use store::DownloadOperationState;

use super::plugin_http_support::{ADMIN_PASS, HttpEnv, TOKEN, assert_err, assert_ok, req_json};
use super::support::{
    StubSearchFetcher, enable_hooks_plugin, minimal_torrent_bytes, nexus_row,
    seed_site_and_subscribe, setup_subscribe_env, spawn_tr_add_stub,
};

struct ReleaseGuard(Option<mpsc::Sender<()>>);
impl Drop for ReleaseGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}

struct BlockingTorrentServer {
    addr: std::net::SocketAddr,
    fetch_count: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl BlockingTorrentServer {
    fn start(body: Vec<u8>) -> (Self, tokio::sync::oneshot::Receiver<()>, mpsc::Sender<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let fetch_count = Arc::new(AtomicUsize::new(0));
        let (ent_tx, ent_rx) = tokio::sync::oneshot::channel();
        let (rel_tx, rel_rx) = mpsc::channel();
        let ent_slot = Mutex::new(Some(ent_tx));
        let rel_slot = Mutex::new(Some(rel_rx));
        let stop_c = Arc::clone(&stop);
        let count_c = Arc::clone(&fetch_count);
        let handle = std::thread::spawn(move || {
            while !stop_c.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let mut buf = [0u8; 4096];
                        let _ = stream.read(&mut buf);
                        count_c.fetch_add(1, Ordering::SeqCst);
                        if let Some(tx) = ent_slot.lock().unwrap().take() {
                            let _ = tx.send(());
                        }
                        if let Some(rx) = rel_slot.lock().unwrap().take() {
                            let _ = rx.recv_timeout(Duration::from_secs(5));
                        }
                        let hdr = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/x-bittorrent\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(hdr.as_bytes());
                        let _ = stream.write_all(&body);
                        let _ = stream.flush();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        (
            Self {
                addr,
                fetch_count,
                stop,
                handle: Some(handle),
            },
            ent_rx,
            rel_tx,
        )
    }

    fn url(&self, name: &str) -> String {
        format!("http://127.0.0.1:{}/{name}", self.addr.port())
    }
}

impl Drop for BlockingTorrentServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn make_blocking_env(
    payload: &str,
    srv_addr: std::net::SocketAddr,
) -> (HttpEnv, Arc<MemoryDownloader>) {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
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
    (env, dl)
}

#[tokio::test]
async fn manual_submit_subscribe_scoped_idempotency_replay_retry_and_mismatch() {
    let ctx = setup_subscribe_env("The.Matrix.1999.1080p.BluRay.x264", "sub-man.torrent").await;
    let url = ctx.srv.url("sub-man.torrent");
    let hdr = [("idempotency-key", "idem-sub-man-1")];
    let body = json!({
        "download_url": url,
        "title": "The.Matrix.1999.1080p",
        "site_id": ctx.site_id.to_string(),
        "subscribe_id": ctx.sub_id.to_string(),
        "force": false,
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
    assert!(
        ctx.env
            .state
            .store()
            .load_pending(ctx.sub_id)
            .unwrap()
            .is_empty()
    );

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
    let pending = ctx.env.state.store().load_pending(ctx.sub_id).unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].1.submitted_at.is_some());

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
    assert_eq!(
        ctx.dl.inner.submitted().len(),
        1,
        "subscribe-scoped replay with force=false must return 200 with zero duplicate submit"
    );

    let (st_mis, _, b_mis) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(TOKEN),
            &hdr,
            Some(json!({
                "download_url": ctx.srv.url("other-sub-man.torrent"),
                "title": "The.Matrix.1999.1080p",
                "site_id": ctx.site_id.to_string(),
                "subscribe_id": ctx.sub_id.to_string(),
                "force": false,
            })),
        ))
        .await;
    assert_err(
        st_mis,
        &b_mis,
        StatusCode::BAD_REQUEST,
        "downloader.add_failed",
    );

    let (site2_id, _) = seed_site_and_subscribe(&ctx.env, &ctx.srv.url("")).await;
    let (st_site_mis, _, b_site_mis) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(TOKEN),
            &hdr,
            Some(json!({
                "download_url": url,
                "title": "The.Matrix.1999.1080p",
                "site_id": site2_id.to_string(),
                "subscribe_id": ctx.sub_id.to_string(),
                "force": false,
            })),
        ))
        .await;
    assert_err(
        st_site_mis,
        &b_site_mis,
        StatusCode::BAD_REQUEST,
        "downloader.add_failed",
    );
}

#[tokio::test]
async fn manual_submit_subscribe_scoped_retries_after_deferred_cleared() {
    let ctx = setup_subscribe_env("The.Matrix.DeferDownload.1080p", "sub-def.torrent").await;
    let pid = "org.example.sub-manual-defer";
    enable_hooks_plugin(&ctx.env, pid, &["download.before"]).await;
    let hdr = [("idempotency-key", "idem-sub-man-defer")];
    let body = json!({
        "download_url": ctx.srv.url("sub-def.torrent"),
        "title": "The.Matrix.DeferDownload.1080p",
        "site_id": ctx.site_id.to_string(),
        "subscribe_id": ctx.sub_id.to_string(),
        "force": false,
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
    assert!(
        ctx.env
            .state
            .store()
            .load_pending(ctx.sub_id)
            .unwrap()
            .is_empty()
    );

    let rev = ctx.env.get_plugin_rev(pid).await;
    ctx.env.disable_plugin(pid, rev, "dis-sub-man-def").await;

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

#[tokio::test]
async fn admit_concurrent_intent_fetch_window_preserves_winner_and_avoids_duplicate_fetch() {
    let (srv, ent_rx, rel_tx) =
        BlockingTorrentServer::start(minimal_torrent_bytes("The.Matrix.1999.1080p", 2048));
    let (env, dl) = make_blocking_env(
        &nexus_row("The.Matrix.1999.1080p", "admit.torrent"),
        srv.addr,
    );
    let (_site_id, sub_id) = seed_site_and_subscribe(&env, &srv.url("")).await;
    let user_id = env
        .state
        .store()
        .get_subscribe(sub_id)
        .unwrap()
        .unwrap()
        .user_id;
    let body = json!({ "subscribe_id": sub_id.to_string(), "enclosure": srv.url("admit.torrent") });
    let hdr = [("idempotency-key", "idem-intent-admit")];
    let env_arc = Arc::new(env);
    let env_bg = Arc::clone(&env_arc);
    let body_bg = body.clone();
    let h1 = tokio::spawn(async move {
        env_bg
            .send(req_json(
                "POST",
                "/api/v1/search/admit",
                Some(TOKEN),
                &[("idempotency-key", "idem-intent-admit")],
                Some(body_bg),
            ))
            .await
    });
    ent_rx.await.unwrap();
    let rel_guard = ReleaseGuard(Some(rel_tx));

    let op = env_arc
        .state
        .store()
        .get_download_operation_by_idempotency(
            &format!("user:{user_id}"),
            "admit-http:idem-intent-admit",
        )
        .unwrap()
        .unwrap();
    assert_eq!(op.state, DownloadOperationState::Intent);

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
    assert_eq!(srv.fetch_count.load(Ordering::SeqCst), 1);

    let (_, sub_other) = seed_site_and_subscribe(&env_arc, &srv.url("")).await;
    let (st_mis, _, b_mis) = env_arc
        .send(req_json(
            "POST",
            "/api/v1/search/admit",
            Some(TOKEN),
            &hdr,
            Some(json!({
                "subscribe_id": sub_other.to_string(),
                "enclosure": srv.url("admit.torrent"),
            })),
        ))
        .await;
    assert_err(st_mis, &b_mis, StatusCode::BAD_REQUEST, "subscribe.invalid");

    drop(rel_guard);
    let (st1, _, b1) = h1.await.unwrap();
    assert_ok(st1, &b1, StatusCode::OK);
    assert_eq!(srv.fetch_count.load(Ordering::SeqCst), 1);
    assert_eq!(dl.submitted().len(), 1);
    let pending = env_arc.state.store().load_pending(sub_id).unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].1.submitted_at.is_some());
}

#[tokio::test]
async fn manual_submit_concurrent_intent_fetch_window_avoids_duplicate_fetch() {
    let (srv, ent_rx, rel_tx) =
        BlockingTorrentServer::start(minimal_torrent_bytes("The.Matrix.1999.1080p", 2048));
    let (env, dl) = make_blocking_env("", srv.addr);
    let (site_id, sub_id) = seed_site_and_subscribe(&env, &srv.url("")).await;
    let body = json!({
        "download_url": srv.url("manual-intent.torrent"),
        "title": "The.Matrix.1999.1080p",
        "site_id": site_id.to_string(),
        "subscribe_id": sub_id.to_string(),
        "force": false,
    });
    let hdr = [("idempotency-key", "idem-intent-manual")];
    let env_arc = Arc::new(env);
    let env_bg = Arc::clone(&env_arc);
    let body_bg = body.clone();
    let h1 = tokio::spawn(async move {
        env_bg
            .send(req_json(
                "POST",
                "/api/v1/downloaders/submit",
                Some(TOKEN),
                &[("idempotency-key", "idem-intent-manual")],
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
    assert_eq!(srv.fetch_count.load(Ordering::SeqCst), 1);
    assert_eq!(dl.submitted().len(), 0);

    drop(rel_guard);
    let (st1, _, b1) = h1.await.unwrap();
    assert_ok(st1, &b1, StatusCode::OK);
    assert_eq!(srv.fetch_count.load(Ordering::SeqCst), 1);
    assert_eq!(dl.submitted().len(), 1);
    let pending = env_arc.state.store().load_pending(sub_id).unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].1.submitted_at.is_some());
}

#[tokio::test]
async fn gate_reject_attributes_rejecting_plugin_id_on_download_operation() {
    let ctx = setup_subscribe_env("The.Matrix.RejectDownload.1080p", "reject-attr.torrent").await;
    let pid = "org.example.rej-attr";
    enable_hooks_plugin(&ctx.env, pid, &["download.before"]).await;
    let user_id = ctx
        .env
        .state
        .store()
        .get_subscribe(ctx.sub_id)
        .unwrap()
        .unwrap()
        .user_id;
    let (st, _, b) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(TOKEN),
            &[("idempotency-key", "idem-rej-attr")],
            Some(json!({
                "download_url": ctx.srv.url("reject-attr.torrent"),
                "title": "The.Matrix.RejectDownload.1080p",
                "site_id": ctx.site_id.to_string(),
            })),
        ))
        .await;
    assert_err(st, &b, StatusCode::BAD_REQUEST, "downloader.add_failed");
    let op = ctx
        .env
        .state
        .store()
        .get_download_operation_by_idempotency(
            &format!("user:{user_id}"),
            "manual-http:idem-rej-attr",
        )
        .unwrap()
        .unwrap();
    assert_eq!(op.state, DownloadOperationState::Rejected);
    assert_eq!(op.plugin_id.as_deref(), Some(pid));
}

#[tokio::test]
async fn admit_and_unpinned_manual_submit_allow_hook_override_with_default_downloader() {
    let dl_a = domain::DownloaderId::new();
    let dl_b = domain::DownloaderId::new();
    let title = format!("The.Matrix.1999.1080p RetargetDl:{dl_b}");
    let ctx = setup_subscribe_env(&title, "retarget-def.torrent").await;
    let (addr_a, adds_a) = spawn_tr_add_stub("1111111111111111111111111111111111111111");
    let (addr_b, adds_b) = spawn_tr_add_stub("2222222222222222222222222222222222222222");
    for (id, name, addr, is_default) in [
        (dl_a, "default-tr", addr_a, true),
        (dl_b, "secondary-tr", addr_b, false),
    ] {
        ctx.env
            .state
            .store()
            .insert_downloader(&api::store::DownloaderRow {
                id,
                name: name.into(),
                kind: "transmission".into(),
                url: format!("http://{addr}/transmission/rpc"),
                username: None,
                password: None,
                api_key: None,
                category: None,
                path_maps: vec![],
                is_default,
                enabled: true,
            })
            .unwrap();
    }
    enable_hooks_plugin(&ctx.env, "org.example.def-retarget", &["download.before"]).await;

    let (st_adm, _, b_adm) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/search/admit",
            Some(TOKEN),
            &[],
            Some(json!({
                "subscribe_id": ctx.sub_id.to_string(),
                "enclosure": ctx.srv.url("retarget-def.torrent")
            })),
        ))
        .await;
    assert_ok(st_adm, &b_adm, StatusCode::OK);
    assert_eq!(adds_a.load(Ordering::SeqCst), 0);
    assert_eq!(adds_b.load(Ordering::SeqCst), 1);

    let (st_man, _, b_man) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(TOKEN),
            &[],
            Some(json!({
                "download_url": ctx.srv.url("submit-retarget.torrent"),
                "title": format!("Clean.2160p RetargetDl:{dl_b}"),
                "site_id": ctx.site_id.to_string()
            })),
        ))
        .await;
    assert_ok(st_man, &b_man, StatusCode::OK);
    assert_eq!(adds_a.load(Ordering::SeqCst), 0);
    assert_eq!(adds_b.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn subscribe_falls_back_from_disabled_downloader() {
    let ctx = setup_subscribe_env("The.Matrix.1999.1080p", "sub-fb.torrent").await;
    let dl_disabled_sub = domain::DownloaderId::new();
    let dl_default = domain::DownloaderId::new();
    let (addr_def, adds_def) = spawn_tr_add_stub("3333333333333333333333333333333333333333");
    for (id, name, url, is_default, enabled) in [
        (
            dl_disabled_sub,
            "disabled-sub-dl",
            "http://127.0.0.1:9/transmission/rpc".to_string(),
            false,
            false,
        ),
        (
            dl_default,
            "enabled-default-dl",
            format!("http://{addr_def}/transmission/rpc"),
            true,
            true,
        ),
    ] {
        ctx.env
            .state
            .store()
            .insert_downloader(&api::store::DownloaderRow {
                id,
                name: name.into(),
                kind: "transmission".into(),
                url,
                username: None,
                password: None,
                api_key: None,
                category: None,
                path_maps: vec![],
                is_default,
                enabled,
            })
            .unwrap();
    }
    let mut sub = ctx
        .env
        .state
        .store()
        .get_subscribe(ctx.sub_id)
        .unwrap()
        .unwrap();
    sub.downloader_id = Some(dl_disabled_sub);
    ctx.env.state.store().update_subscribe(&sub).unwrap();

    let (st_run, _, b_run) = ctx
        .env
        .send(req_json(
            "POST",
            &format!("/api/v1/subscriptions/{}/run", ctx.sub_id),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_ok(st_run, &b_run, StatusCode::OK);
    assert_eq!(ctx.dl.inner.submitted().len(), 1);
    let pending = ctx.env.state.store().load_pending(ctx.sub_id).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].1.downloader_id, Some(dl_default));
    let _ = adds_def;
}

#[tokio::test]
async fn disabled_manual_submit_detects_url_mutation_via_endpoint_fingerprint() {
    let (addr_def, adds_def) = spawn_tr_add_stub("4444444444444444444444444444444444444444");
    let (srv, ent_rx, rel_tx) =
        BlockingTorrentServer::start(minimal_torrent_bytes("Mut.1080p", 2048));
    let rel_guard = ReleaseGuard(Some(rel_tx));
    let (env2, _) = make_blocking_env(&nexus_row("Mut.1080p", "mut.torrent"), srv.addr);
    let (site2, _) = seed_site_and_subscribe(&env2, &srv.url("")).await;
    let dl_mut = domain::DownloaderId::new();
    env2.state
        .store()
        .insert_downloader(&api::store::DownloaderRow {
            id: dl_mut,
            name: "mut-dl".into(),
            kind: "transmission".into(),
            url: "http://127.0.0.1:9/old-rpc".into(),
            username: None,
            password: None,
            api_key: None,
            category: None,
            path_maps: vec![],
            is_default: false,
            enabled: false,
        })
        .unwrap();
    let env_arc = Arc::new(env2);
    let env_req = Arc::clone(&env_arc);
    let req_url = srv.url("mut.torrent");
    let handle = tokio::spawn(async move {
        env_req
            .send(req_json(
                "POST",
                "/api/v1/downloaders/submit",
                Some(TOKEN),
                &[("idempotency-key", "idem-mut-disabled")],
                Some(json!({
                    "download_url": req_url,
                    "title": "Mut.1080p",
                    "site_id": site2.to_string(),
                    "downloader_id": dl_mut.to_string()
                })),
            ))
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), ent_rx)
        .await
        .unwrap()
        .unwrap();
    let new_url = format!("http://{addr_def}/transmission/rpc");
    env_arc
        .state
        .store()
        .mutate_downloader(dl_mut, false, |r| {
            r.url = new_url;
            r.enabled = true;
            Ok(())
        })
        .unwrap();
    drop(rel_guard);

    let (st_mut, _, b_mut) = handle.await.unwrap();
    assert_err(
        st_mut,
        &b_mut,
        StatusCode::BAD_REQUEST,
        "downloader.add_failed",
    );
    assert_eq!(
        adds_def.load(Ordering::SeqCst),
        0,
        "retargeted disabled downloader must not submit to mutated URL"
    );
}
