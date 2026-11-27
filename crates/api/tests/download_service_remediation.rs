#[path = "download_service_remediation/extra.rs"]
mod extra;
#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;
#[path = "download_service_remediation/support.rs"]
mod support;

#[path = "support/lifecycle.rs"]
#[allow(dead_code)]
mod lifecycle;

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use api::{ApiDownloadService, DownloadOutcome, DownloadSubmitError, DownloadSubmitter};
use axum::http::StatusCode;
use domain::{
    AtomRule, Coverage, DownloaderId, FetchMode, Filter, FilterAtom, FilterId, Media, MediaId,
    MediaKind, SiteId, Subscribe, SubscribeFacts, SubscribeId, UserId,
};
use lifecycle::FakeServer;
use serde_json::json;
use store::{
    AcceptDownloadOperationInput, DownloadOperationState, SubscribeDeferState,
    TransitionDownloadOperationInput,
};

use plugin_http_support::{TOKEN, assert_ok, req_json};
use support::*;

fn spawn_tx_counter_server(hits: Arc<AtomicUsize>) -> FakeServer {
    FakeServer::start(move |req, stream| {
        let body = if req.raw.contains("session-get") {
            r#"{"result":"success","arguments":{"rpc-version":16}}"#.to_string()
        } else if req.raw.contains("torrent-add") {
            hits.fetch_add(1, Ordering::SeqCst);
            r#"{"result":"success","arguments":{"torrent-added":{"id":1,"hashString":"0123456789abcdef0123456789abcdef01234567"}}}"#.to_string()
        } else {
            r#"{"result":"success","arguments":{"torrents":[]}}"#.to_string()
        };
        let resp = format!(
            "HTTP/1.1 200 OK\r\nX-Transmission-Session-Id: s1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    })
}

#[tokio::test]
async fn download_before_cannot_retarget_explicit_or_frozen_downloader_id() {
    let (bytes, _) = minimal_torrent_bytes("Frozen.Dl", 2048);
    let srv = StubHttpServer::start(bytes);
    let dl = Arc::new(RecordingDownloader::new(
        tempfile::tempdir().unwrap().keep(),
    ));
    let env = make_env(
        dl.clone(),
        Some(net_guard::TargetPolicy::production().with_test_loopback_socket(srv.addr)),
    );
    let hits1 = Arc::new(AtomicUsize::new(0));
    let hits2 = Arc::new(AtomicUsize::new(0));
    let tx1 = spawn_tx_counter_server(Arc::clone(&hits1));
    let tx2 = spawn_tx_counter_server(Arc::clone(&hits2));
    let dl1 = DownloaderId::new();
    let dl2 = DownloaderId::new();
    insert_tx_downloader(
        env.state.store().as_ref(),
        dl1,
        &format!("{}/transmission/rpc", tx1.url()),
        false,
    );
    insert_tx_downloader(
        env.state.store().as_ref(),
        dl2,
        &format!("{}/transmission/rpc", tx2.url()),
        false,
    );
    install_dl_hooks(
        &env,
        "org.example.retarget",
        &["hooks:download.before"],
        "[[components.hooks]]\nid = \"download.before\"\npriority = 10\n",
    )
    .await;

    let svc = ApiDownloadService::new(env.state.clone());
    let torrent = test_torrent(
        SiteId::new(),
        None,
        &format!("Movie.RetargetDl:{dl2}"),
        &srv.url("retarget.torrent"),
    );
    let mut plan = default_plan(torrent, api::SubmissionPurpose::Plugin);
    plan.downloader_id = Some(dl1);
    plan.frozen_endpoint = Some(format!("transmission|{}/transmission/rpc", tx1.url()));
    let out = svc.submit(plan, op_ctx("retarget-1")).await.unwrap();
    assert!(matches!(out, DownloadOutcome::Submitted { downloader_id: Some(id), .. } if id == dl1));
    assert_eq!(hits1.load(Ordering::SeqCst), 1, "explicit dl1 must be used");
    assert_eq!(
        hits2.load(Ordering::SeqCst),
        0,
        "download.before must NOT retarget to dl2"
    );
}

#[tokio::test]
async fn download_service_attributes_defer_plugin_and_propagates_11th_attempt_rejected() {
    let dl = Arc::new(RecordingDownloader::new(
        tempfile::tempdir().unwrap().keep(),
    ));
    let env = make_env(dl, None);
    let filter_id = FilterId::new();
    env.state
        .store()
        .insert_filter(&Filter {
            id: filter_id,
            name: "f".into(),
            atoms: vec![FilterAtom {
                priority: 100,
                rule: AtomRule::TitleMatch("Defer".into()),
                exclude: false,
            }],
            keep_old_versions: false,
        })
        .unwrap();
    let media_id = MediaId::new();
    env.state
        .store()
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Movie,
            title: "Defer Test".into(),
            year: Some(2024),
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let sub_id = SubscribeId::new();
    env.state
        .store()
        .insert_subscribe(&Subscribe {
            id: sub_id,
            user_id: UserId::new(),
            media_id,
            coverage: Coverage::Movie,
            fetch_mode: FetchMode::Search,
            filter_id,
            wash_cut: false,
            keep_old_versions: false,
            wash_cut_filter_id: None,
            full_season_pack: false,
            downloader_id: None,
            library_id: None,
            tracking_state: "active".into(),
            follow_future: false,
            search_interval_secs: 1800,
        })
        .unwrap();
    install_dl_hooks(
        &env,
        "org.example.dl-defer",
        &["hooks:download.before"],
        "[[components.hooks]]\nid = \"download.before\"\npriority = 10\n",
    )
    .await;

    let svc = ApiDownloadService::new(env.state.clone());
    let torrent = test_torrent(
        SiteId::new(),
        None,
        "Movie.DeferDownload.1080p",
        "https://pt.example/defer.torrent",
    );
    let mut plan = default_plan(torrent, api::SubmissionPurpose::Subscribe);
    plan.subscribe_id = Some(sub_id);

    for attempt in 1..=10 {
        let out = svc
            .submit(plan.clone(), op_ctx(&format!("def-{attempt}")))
            .await
            .unwrap();
        match out {
            DownloadOutcome::Deferred { reason, .. } => {
                assert!(!reason.contains("SECRET_DEFER_PASS"), "{reason}");
            }
            other => panic!("attempt {attempt} expected Deferred, got {other:?}"),
        }
    }
    let defers = env.state.store().list_subscribe_defers(sub_id, 10).unwrap();
    assert_eq!(defers.len(), 1);
    assert_eq!(defers[0].plugin_id.as_deref(), Some("org.example.dl-defer"));
    assert_eq!(defers[0].state, SubscribeDeferState::Deferred);

    let ctx11 = op_ctx("def-11");
    let out11 = svc.submit(plan.clone(), ctx11.clone()).await.unwrap();
    assert!(
        matches!(out11, DownloadOutcome::Rejected { .. }),
        "11th attempt: {out11:?}"
    );
    assert_eq!(
        env.state.store().list_subscribe_defers(sub_id, 10).unwrap()[0].state,
        SubscribeDeferState::Rejected
    );
    assert_eq!(
        env.state
            .store()
            .get_download_operation(&ctx11.operation_id.to_string())
            .unwrap()
            .unwrap()
            .state,
        DownloadOperationState::Rejected
    );
    let out12 = svc.submit(plan, op_ctx("def-12")).await.unwrap();
    assert!(
        matches!(out12, DownloadOutcome::Rejected { .. }),
        "12th attempt: {out12:?}"
    );
}

fn run_subscribe_admit_once<'a>(
    sub: &'a Subscribe,
    media: &'a Media,
    filter: &'a Filter,
    dl: &'a RecordingDownloader,
    svc: &'a ApiDownloadService,
    lib_root: &'a std::path::Path,
    t: domain::Torrent,
) -> subscribe::Added<'a, RecordingDownloader> {
    subscribe::admit_and_add(subscribe::RunInput {
        subscribe: sub,
        media,
        filter,
        wash_filter: None,
        torrents: vec![t],
        search_keywords: vec![],
        facts: SubscribeFacts::default(),
        downloader: dl,
        library_root: lib_root,
        library_id: None,
        root_id: None,
        config_revision: None,
        transfer_mode: None,
        scrape: false,
        hooks: None,
        policy_hooks: None,
        submitter: Some(svc),
        naming: None,
        preserve_removed: false,
        overlap: Default::default(),
        absolute: None,
        renamed_destinations: Default::default(),
    })
    .unwrap()
}

fn seed_matrix_subscribe(store: &api::Store) -> (Media, Filter, Subscribe) {
    let filter = Filter {
        id: FilterId::new(),
        name: "f".into(),
        atoms: vec![FilterAtom {
            priority: 100,
            rule: AtomRule::TitleMatch("Matrix".into()),
            exclude: false,
        }],
        keep_old_versions: false,
    };
    store.insert_filter(&filter).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: Some(1999),
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let sub = Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id: media.id,
        coverage: Coverage::Movie,
        fetch_mode: FetchMode::Search,
        filter_id: filter.id,
        wash_cut: false,
        keep_old_versions: false,
        wash_cut_filter_id: None,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    };
    store.insert_subscribe(&sub).unwrap();
    (media, filter, sub)
}

#[tokio::test]
async fn subscribe_and_admit_retry_after_defer_and_fetch_error_and_resolve_defer() {
    let (bytes, _) = minimal_torrent_bytes("The.Matrix.1999.1080p", 4096);
    let srv = StubHttpServer::start(bytes);
    *srv.status.lock().unwrap() = 500;
    let dl = Arc::new(RecordingDownloader::new(
        tempfile::tempdir().unwrap().keep(),
    ));
    let env = make_env(
        dl.clone(),
        Some(net_guard::TargetPolicy::production().with_test_loopback_socket(srv.addr)),
    );
    let (media, filter, sub) = seed_matrix_subscribe(env.state.store().as_ref());

    let svc = ApiDownloadService::new(env.state.clone());
    let torrent = test_torrent(
        SiteId::new(),
        None,
        "The.Matrix.1999.1080p",
        &srv.url("m.torrent"),
    );
    let lib_dir = tempfile::tempdir().unwrap();
    let r1 = run_subscribe_admit_once(
        &sub,
        &media,
        &filter,
        &dl,
        &svc,
        lib_dir.path(),
        torrent.clone(),
    );
    assert!(!r1.add_errors.is_empty());

    *srv.status.lock().unwrap() = 200;
    let r2 = run_subscribe_admit_once(
        &sub,
        &media,
        &filter,
        &dl,
        &svc,
        lib_dir.path(),
        torrent.clone(),
    );
    assert!(
        r2.add_errors.is_empty(),
        "retry after 500 must succeed: {:?}",
        r2.add_errors
    );
    assert_eq!(r2.chosen.len(), 1);

    let binding = env
        .state
        .store()
        .subscribe_policy_binding(sub.id)
        .unwrap()
        .unwrap();
    env.state
        .store()
        .record_subscribe_defer(&store::RecordSubscribeDeferInput {
            defer_id: uuid::Uuid::new_v4().to_string(),
            subscribe_id: sub.id,
            subscribe_generation: binding.jobs_sync_generation,
            policy_revision: binding.policy_revision,
            enclosure: torrent.enclosure.clone(),
            info_hash: None,
            plugin_id: Some("p1".into()),
            hook_id: "download.before".into(),
            reason: "wait".into(),
            next_retry_at: 0,
            now_utc: 100,
        })
        .unwrap();
    let r3 = run_subscribe_admit_once(&sub, &media, &filter, &dl, &svc, lib_dir.path(), torrent);
    assert_eq!(r3.chosen.len(), 1);
    assert_eq!(
        env.state.store().list_subscribe_defers(sub.id, 10).unwrap()[0].state,
        SubscribeDeferState::Resolved
    );
    let _ = (TOKEN, StatusCode::OK, assert_ok, req_json, json!({}));
}

#[tokio::test]
async fn replay_outcome_prevents_duplicate_add_for_submitted_unknown_and_submitting() {
    let (bytes, _) = minimal_torrent_bytes("Replay.Guard", 2048);
    let srv = StubHttpServer::start(bytes);
    let dl = Arc::new(RecordingDownloader::new(
        tempfile::tempdir().unwrap().keep(),
    ));
    let env = make_env(
        dl.clone(),
        Some(net_guard::TargetPolicy::production().with_test_loopback_socket(srv.addr)),
    );
    let svc = ApiDownloadService::new(env.state.clone());
    let dl_id = DownloaderId::from_uuid(uuid::Uuid::nil());
    let torrent = test_torrent(SiteId::new(), None, "Replay.Guard", &srv.url("rep.torrent"));
    let plan = default_plan(torrent.clone(), api::SubmissionPurpose::Manual);
    let ctx = op_ctx("replay-submitting-key");

    // Pre-seed a Submitting row with matching (subject, idempotency_key, request_digest)
    let first = svc.submit(plan.clone(), op_ctx("replay-ok")).await.unwrap();
    assert!(matches!(first, DownloadOutcome::Submitted { .. }));
    let op_row = env
        .state
        .store()
        .get_download_operation_by_idempotency("user:test", "replay-ok")
        .unwrap()
        .unwrap();
    let digest = op_row.request_digest.clone();

    let sub_op = env
        .state
        .store()
        .accept_download_operation(&AcceptDownloadOperationInput {
            operation_id: ctx.operation_id.to_string(),
            idempotency_key: ctx.idempotency_key.clone(),
            source_kind: "manual".into(),
            subscribe_id: None,
            plugin_id: None,
            subject: ctx.subject.clone(),
            cause_id: ctx.cause_id.clone(),
            site_id: Some(torrent.site_id),
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            enclosure: torrent.enclosure.clone(),
            ownership_tag: format!("cm-op:{}", ctx.operation_id),
            info_hash: None,
            bytes_digest: None,
            verified_bytes: None,
            request_digest: digest,
            now_utc: 100,
        })
        .unwrap()
        .operation;
    env.state
        .store()
        .transition_download_operation(&TransitionDownloadOperationInput {
            operation_id: sub_op.operation_id,
            expected_revision: sub_op.revision,
            next_state: DownloadOperationState::Submitting,
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            info_hash: None,
            bytes_digest: None,
            verified_bytes: None,
            outcome_code: None,
            error_message: None,
            submitted_at: None,
            now_utc: 101,
        })
        .unwrap();

    let count_before = dl.submit_count.load(Ordering::SeqCst);
    let replayed = svc.submit(plan, ctx).await.unwrap();
    assert!(
        matches!(replayed, DownloadOutcome::Unknown { .. }),
        "Submitting replay must return Unknown: {replayed:?}"
    );
    assert_eq!(dl.submit_count.load(Ordering::SeqCst), count_before);
    let _ = DownloadSubmitError::Cancelled;
}

#[tokio::test]
async fn replay_outcome_handles_intent_and_reserved_and_digest_includes_tags() {
    let (bytes, _) = minimal_torrent_bytes("Replay.IntentReserved", 2048);
    let srv = StubHttpServer::start(bytes);
    let dl = Arc::new(RecordingDownloader::new(
        tempfile::tempdir().unwrap().keep(),
    ));
    let env = make_env(
        dl.clone(),
        Some(net_guard::TargetPolicy::production().with_test_loopback_socket(srv.addr)),
    );
    let svc = ApiDownloadService::new(env.state.clone());
    let dl_id = DownloaderId::from_uuid(uuid::Uuid::nil());
    let torrent = test_torrent(SiteId::new(), None, "Replay.IR", &srv.url("ir.torrent"));
    let mut plan = default_plan(torrent.clone(), api::SubmissionPurpose::Manual);
    plan.tags = vec!["tag-a".into()];

    let first = svc
        .submit(plan.clone(), op_ctx("ir-digest-src"))
        .await
        .unwrap();
    assert!(matches!(first, DownloadOutcome::Submitted { .. }));
    let digest = env
        .state
        .store()
        .get_download_operation_by_idempotency("user:test", "ir-digest-src")
        .unwrap()
        .unwrap()
        .request_digest;

    let mut diff_tags_plan = plan.clone();
    diff_tags_plan.tags = vec!["tag-b".into()];
    let err = svc
        .submit(diff_tags_plan, op_ctx("ir-digest-src"))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("idempotency"),
        "tags mismatch must trigger IdempotencyConflict: {err}"
    );

    let ctx_int = op_ctx("ir-intent-key");
    env.state
        .store()
        .accept_download_operation(&AcceptDownloadOperationInput {
            operation_id: ctx_int.operation_id.to_string(),
            idempotency_key: ctx_int.idempotency_key.clone(),
            source_kind: "manual".into(),
            subscribe_id: None,
            plugin_id: None,
            subject: ctx_int.subject.clone(),
            cause_id: ctx_int.cause_id.clone(),
            site_id: Some(torrent.site_id),
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            enclosure: torrent.enclosure.clone(),
            ownership_tag: format!("cm-op:{}", ctx_int.operation_id),
            info_hash: None,
            bytes_digest: None,
            verified_bytes: None,
            request_digest: digest.clone(),
            now_utc: 100,
        })
        .unwrap();
    let before = dl.submit_count.load(Ordering::SeqCst);
    let out_int = svc.submit(plan.clone(), ctx_int).await.unwrap();
    assert!(matches!(out_int, DownloadOutcome::Unknown { .. }));
    assert_eq!(dl.submit_count.load(Ordering::SeqCst), before);

    let ctx_res = op_ctx("ir-reserved-key");
    let op_res = env
        .state
        .store()
        .accept_download_operation(&AcceptDownloadOperationInput {
            operation_id: ctx_res.operation_id.to_string(),
            idempotency_key: ctx_res.idempotency_key.clone(),
            source_kind: "manual".into(),
            subscribe_id: None,
            plugin_id: None,
            subject: ctx_res.subject.clone(),
            cause_id: ctx_res.cause_id.clone(),
            site_id: Some(torrent.site_id),
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            enclosure: torrent.enclosure.clone(),
            ownership_tag: format!("cm-op:{}", ctx_res.operation_id),
            info_hash: None,
            bytes_digest: None,
            verified_bytes: None,
            request_digest: digest,
            now_utc: 100,
        })
        .unwrap()
        .operation;
    env.state
        .store()
        .transition_download_operation(&TransitionDownloadOperationInput {
            operation_id: op_res.operation_id,
            expected_revision: op_res.revision,
            next_state: DownloadOperationState::Reserved,
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            info_hash: None,
            bytes_digest: None,
            verified_bytes: None,
            outcome_code: None,
            error_message: None,
            submitted_at: None,
            now_utc: 101,
        })
        .unwrap();
    let out_res = svc.submit(plan, ctx_res).await.unwrap();
    assert!(matches!(out_res, DownloadOutcome::Unknown { .. }));
    assert_eq!(dl.submit_count.load(Ordering::SeqCst), before);
}
