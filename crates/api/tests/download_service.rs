#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use api::{
    ApiDownloadService, ApiState, DownloadOpContext, DownloadOutcome, DownloadSubmitError,
    DownloadSubmitter, FrozenDownloadPlan, Store,
};
use domain::{DownloaderId, PluginOperationId, Site, SiteId, SubscribeId, Torrent};
use downloader::{
    ClientSubmitResult, ClientTaskQuery, Downloader, DownloaderError, MemoryDownloader,
    SubmitOptions, SubmitPayload, TaskSnapshot,
};
use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
use plugin_api::HOOK_WORLD;
use plugin_http_support::{ADMIN_PASS, HttpEnv, TOKEN, pkg};
use serde_json::json;
use sha2::Digest;
use store::plugins::{PluginCommandCause, PluginOperationState, PluginSubject};
use store::{
    DownloadOperationState, DownloadOwnershipState, DownloadProvenanceKind, ManualCredentialWrite,
};

const POLICY_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/policy.wasm");

struct NoopFetcher;
impl Fetcher for NoopFetcher {
    fn fetch(&self, _req: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

fn minimal_torrent_bytes(name: &str, length: usize) -> (Vec<u8>, String) {
    let info_dict = format!(
        "d6:lengthi{length}e4:name{}:{name}12:piece lengthi16384e6:pieces20:01234567890123456789e",
        name.len()
    );
    let mut bytes = Vec::from(b"d8:announce20:https://tracker/test4:info");
    bytes.extend_from_slice(info_dict.as_bytes());
    bytes.push(b'e');
    let hash = downloader::torrent_info_hash(&bytes).unwrap();
    (bytes, hash)
}

struct StubTorrentServer {
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StubTorrentServer {
    fn start(payload: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_c = stop.clone();
        let handle = std::thread::spawn(move || {
            while !stop_c.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let mut buf = [0u8; 2048];
                        let _ = stream.read(&mut buf);
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/x-bittorrent\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            payload.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(&payload);
                        let _ = stream.flush();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            port,
            stop,
            handle: Some(handle),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}/{path}", self.port)
    }

    fn addr(&self) -> std::net::SocketAddr {
        std::net::SocketAddr::from(([127, 0, 0, 1], self.port))
    }
}

impl Drop for StubTorrentServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

struct RecordingDownloader {
    inner: MemoryDownloader,
    submit_count: AtomicUsize,
    query_mode: Mutex<QueryMode>,
    unknown_mode: Mutex<UnknownMode>,
    on_submit: Option<Arc<dyn Fn() + Send + Sync>>,
}

#[derive(Clone)]
enum QueryMode {
    PassThrough,
    Foreign(String),
    NotFound,
    Unreachable,
}

#[derive(Clone, Copy)]
enum UnknownMode {
    Off,
    PostSendAccepted,
    PreSendDropped,
}

impl RecordingDownloader {
    fn new(stage: PathBuf) -> Self {
        Self {
            inner: MemoryDownloader::new(stage),
            submit_count: AtomicUsize::new(0),
            query_mode: Mutex::new(QueryMode::PassThrough),
            unknown_mode: Mutex::new(UnknownMode::Off),
            on_submit: None,
        }
    }

    fn with_on_submit(mut self, f: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.on_submit = Some(f);
        self
    }

    fn set_query_mode(&self, mode: QueryMode) {
        *self.query_mode.lock().unwrap() = mode;
    }

    fn set_unknown_mode(&self, mode: UnknownMode) {
        *self.unknown_mode.lock().unwrap() = mode;
    }
}

impl Downloader for RecordingDownloader {
    fn remember_identity(&self, torrent: &Torrent, hash: &str) -> Result<(), DownloaderError> {
        self.inner.remember_identity(torrent, hash)
    }
    fn select_files(
        &self,
        torrent: &Torrent,
        keep: &dyn Fn(&str) -> bool,
    ) -> Result<Option<usize>, DownloaderError> {
        self.inner.select_files(torrent, keep)
    }
    fn submit_verified(
        &self,
        payload: &SubmitPayload,
        options: &SubmitOptions,
    ) -> Result<ClientSubmitResult, DownloaderError> {
        if let Some(cb) = &self.on_submit {
            cb();
        }
        self.submit_count.fetch_add(1, Ordering::SeqCst);
        let umode = *self.unknown_mode.lock().unwrap();
        match umode {
            UnknownMode::Off => self.inner.submit_verified(payload, options),
            UnknownMode::PostSendAccepted => {
                let _ = self.inner.submit_verified(payload, options)?;
                Ok(ClientSubmitResult::UnknownOutcome {
                    reason: "post-send response dropped".into(),
                })
            }
            UnknownMode::PreSendDropped => Ok(ClientSubmitResult::UnknownOutcome {
                reason: "pre-send connection lost".into(),
            }),
        }
    }
    fn query_task_status(
        &self,
        info_hash: &str,
        ownership_tag: &str,
    ) -> Result<ClientTaskQuery, DownloaderError> {
        let mode = self.query_mode.lock().unwrap().clone();
        match mode {
            QueryMode::PassThrough => self.inner.query_task_status(info_hash, ownership_tag),
            QueryMode::Foreign(h) => Ok(ClientTaskQuery {
                exists: true,
                info_hash: Some(h),
                owned_by_tag: false,
                broken: false,
                state: None,
                paused: false,
            }),
            QueryMode::NotFound => Ok(ClientTaskQuery::default()),
            QueryMode::Unreachable => Err(DownloaderError::Message("connection refused".into())),
        }
    }
    fn supports_labels(&self) -> bool {
        true
    }
    fn completed_files(&self, torrent: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        self.inner.completed_files(torrent)
    }
    fn is_claimed(&self, torrent: &Torrent) -> Result<bool, DownloaderError> {
        self.inner.is_claimed(torrent)
    }
    fn collectable_files(
        &self,
        torrent: &Torrent,
    ) -> Result<(Vec<PathBuf>, bool), DownloaderError> {
        self.inner.collectable_files(torrent)
    }
    fn uploaded_by_category(&self, category: &str) -> Result<u64, DownloaderError> {
        self.inner.uploaded_by_category(category)
    }
    fn remove(&self, torrent: &Torrent, delete_files: bool) -> Result<(), DownloaderError> {
        self.inner.remove(torrent, delete_files)
    }
    fn delete_task(&self, info_hash: &str, delete_files: bool) -> Result<(), DownloaderError> {
        self.inner.delete_task(info_hash, delete_files)
    }
    fn pause_task(&self, info_hash: &str) -> Result<(), DownloaderError> {
        self.inner.pause_task(info_hash)
    }
    fn resume_task(&self, info_hash: &str) -> Result<(), DownloaderError> {
        self.inner.resume_task(info_hash)
    }
    fn task_snapshots(&self) -> Result<Vec<TaskSnapshot>, DownloaderError> {
        self.inner.task_snapshots()
    }
}

fn build_policy_pkg(id: &str, hooks_toml: &str, perms: &[&str]) -> Vec<u8> {
    let perms_str = perms
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let schema = pkg::sample_schema(false, None);
    let manifest = format!(
        r#"manifest_version = 1
id = "{id}"
name = "Policy {id}"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["test"]
description = "Hook fixture"
config_schema = "config.schema.json"
state_schema = 1

[[components]]
id = "policy"
entry = "policy.wasm"
world = "{HOOK_WORLD}"
permissions = [{perms_str}]

{hooks_toml}
"#
    );
    pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", schema.as_bytes()),
        ("policy.wasm", POLICY_WASM),
    ])
}

fn test_site(id: SiteId) -> Site {
    Site {
        id,
        name: "TestSite".into(),
        url: "https://site.example/".into(),
        profile_id: "mteam".into(),
        cookie: None,
        api_key: None,
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: Some(60),
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: domain::MaintenanceVia::Http,
        browser_search: false,
        timezone: "UTC".into(),
    }
}

fn test_torrent(site_id: SiteId, id: &str, enclosure: &str, claimed_size: Option<u64>) -> Torrent {
    Torrent {
        site_id,
        id: Some(id.into()),
        title: "Sample.Movie.2024.1080p".into(),
        enclosure: enclosure.into(),
        size_bytes: claimed_size,
        seeders: Some(10),
        leechers: Some(1),
        snatched: None,
        upload_time: None,
        poster_url: None,
        free: true,
        hr: Some(false),
        imdb_id: None,
        detail_url: None,
        category: None,
    }
}

fn default_plan(torrent: Torrent) -> FrozenDownloadPlan {
    let source = downloader::TorrentSource::from_url(&torrent.enclosure).unwrap();
    FrozenDownloadPlan {
        torrent,
        source,
        purpose: downloader::SubmissionPurpose::Manual,
        downloader_id: None,
        frozen_endpoint: None,
        subscribe_id: None,
        subscribe_generation: None,
        policy_revision: None,
        media_id: None,
        score: None,
        release_override: None,
        force: false,
        save_path: None,
        category: None,
        tags: Vec::new(),
        paused: false,
        upload_limit: None,
        download_limit: None,
        sequential: false,
        first_last_piece: false,
        budget_key: None,
        plan_digest: None,
    }
}

fn op_ctx(idempotency_key: &str) -> DownloadOpContext {
    DownloadOpContext {
        operation_id: PluginOperationId::new(),
        cause_id: format!("cause-{idempotency_key}"),
        subject: "user:admin".into(),
        idempotency_key: idempotency_key.into(),
        plugin_id: None,
        generation: None,
        config_revision: None,
        dry_run: false,
    }
}

#[tokio::test]
async fn download_service_executes_verified_flow_releases_locks_and_blocks_untrusted_sizes() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let store_for_cb = Arc::clone(&store);
    let sub_id = SubscribeId::new();
    let lock_ok = Arc::new(AtomicBool::new(false));
    let lock_ok_c = Arc::clone(&lock_ok);
    let state_holder: Arc<Mutex<Option<ApiState>>> = Arc::new(Mutex::new(None));
    let state_holder_c = Arc::clone(&state_holder);
    let dl = Arc::new(
        RecordingDownloader::new(dir.path().join("stage")).with_on_submit(Arc::new(move || {
            let _ = store_for_cb.list_unresolved_download_operations(None, Some(10));
            if let Some(st) = state_holder_c.lock().unwrap().as_ref() {
                assert!(
                    st.subscribe_guard(sub_id).try_lock().is_some(),
                    "subscribe_guard must not be held during submit_verified"
                );
                assert!(
                    st.library_work_idle_for_test(),
                    "LibraryWorkLease must not be held during submit_verified"
                );
            }
            lock_ok_c.store(true, Ordering::SeqCst);
        })),
    );
    let (torrent_bytes, expected_hash) = minimal_torrent_bytes("Sample.Movie.2024", 4096);
    let srv = StubTorrentServer::start(torrent_bytes);
    let state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl.clone(),
    )
    .unwrap()
    .with_plugin_http_policy_override(
        net_guard::TargetPolicy::production().with_test_loopback_socket(srv.addr()),
    );
    *state_holder.lock().unwrap() = Some(state.clone());

    let site_id = SiteId::new();
    state
        .store()
        .save_site(&test_site(site_id), ManualCredentialWrite::Replace)
        .unwrap();
    let torrent = test_torrent(site_id, "t-1", &srv.url("1.torrent"), Some(999_999));
    let svc = ApiDownloadService::new(state);
    let mut plan = default_plan(torrent);
    plan.tags = vec!["tag-1".into()];

    let outcome = svc
        .submit(plan.clone(), op_ctx("dl-service-1"))
        .await
        .unwrap();
    match outcome {
        DownloadOutcome::Submitted {
            info_hash,
            verified_total_bytes,
            ..
        } => {
            assert_eq!(info_hash.as_deref(), Some(expected_hash.as_str()));
            assert_eq!(verified_total_bytes, Some(4096));
        }
        other => panic!("expected Submitted, got {other:?}"),
    }
    assert!(lock_ok.load(Ordering::SeqCst));
    assert_eq!(dl.submit_count.load(Ordering::SeqCst), 1);

    plan.purpose = downloader::SubmissionPurpose::Boost;
    plan.source = downloader::TorrentSource::from_url(
        "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567",
    )
    .unwrap();
    let boost_err = svc
        .submit(plan, op_ctx("dl-service-boost-magnet"))
        .await
        .unwrap_err();
    assert!(matches!(boost_err, DownloadSubmitError::Fetch(_)));
}

#[tokio::test]
async fn download_service_reconciles_unknown_on_restart() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let dl = Arc::new(RecordingDownloader::new(dir.path().join("stage")));
    let (torrent_bytes, expected_hash) = minimal_torrent_bytes("Unknown.Reconcile", 2048);
    let srv = StubTorrentServer::start(torrent_bytes);
    let state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl.clone(),
    )
    .unwrap()
    .with_plugin_http_policy_override(
        net_guard::TargetPolicy::production().with_test_loopback_socket(srv.addr()),
    );

    dl.set_unknown_mode(UnknownMode::PostSendAccepted);
    let svc = ApiDownloadService::new(state.clone());
    let ctx = op_ctx("unk-op-1");
    let op_id = ctx.operation_id;
    let plan = default_plan(test_torrent(
        SiteId::new(),
        "u-1",
        &srv.url("unk.torrent"),
        None,
    ));
    let out = svc.submit(plan, ctx).await.unwrap();
    assert!(matches!(out, DownloadOutcome::Unknown { .. }));
    assert_eq!(dl.submit_count.load(Ordering::SeqCst), 1);

    let stored_op = state
        .store()
        .get_download_operation(&op_id.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(stored_op.state, DownloadOperationState::Unknown);
    let dl_id = stored_op.downloader_id.unwrap();
    let own = state
        .store()
        .get_download_ownership(dl_id, &stored_op.ownership_tag)
        .unwrap()
        .unwrap();
    assert_eq!(own.ownership_state, DownloadOwnershipState::Unproven);

    dl.set_query_mode(QueryMode::Unreachable);
    api::recover_plugin_startup_for_test(&state).unwrap();
    assert_eq!(
        state
            .store()
            .get_download_operation(&op_id.to_string())
            .unwrap()
            .unwrap()
            .state,
        DownloadOperationState::Unknown
    );

    dl.set_query_mode(QueryMode::PassThrough);
    dl.set_unknown_mode(UnknownMode::Off);
    api::recover_plugin_startup_for_test(&state).unwrap();
    assert_eq!(dl.submit_count.load(Ordering::SeqCst), 1);
    let after = state
        .store()
        .get_download_operation(&op_id.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(after.state, DownloadOperationState::Submitted);
    assert_eq!(after.info_hash.as_deref(), Some(expected_hash.as_str()));
    let after_own = state
        .store()
        .get_download_ownership(dl_id, &stored_op.ownership_tag)
        .unwrap()
        .unwrap();
    assert_eq!(after_own.ownership_state, DownloadOwnershipState::Owned);
}

#[tokio::test]
async fn download_service_reconciles_foreign_and_not_found_from_unknown() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let dl = Arc::new(RecordingDownloader::new(dir.path().join("stage")));
    let (torrent_bytes, expected_hash) = minimal_torrent_bytes("Foreign.And.NotFound", 2048);
    let srv = StubTorrentServer::start(torrent_bytes);
    let state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl.clone(),
    )
    .unwrap()
    .with_plugin_http_policy_override(
        net_guard::TargetPolicy::production().with_test_loopback_socket(srv.addr()),
    );

    let svc = ApiDownloadService::new(state.clone());
    dl.set_unknown_mode(UnknownMode::PreSendDropped);
    let ctx_f = op_ctx("unk-op-foreign");
    let op_f = ctx_f.operation_id;
    let _ = svc
        .submit(
            default_plan(test_torrent(
                SiteId::new(),
                "f-1",
                &srv.url("f.torrent"),
                None,
            )),
            ctx_f,
        )
        .await
        .unwrap();
    dl.set_query_mode(QueryMode::Foreign(expected_hash));
    api::reconcile_plugin_pending_for_test(&state).unwrap();
    let rec_f = state
        .store()
        .get_download_operation(&op_f.to_string())
        .unwrap()
        .unwrap();
    let dl_id = rec_f.downloader_id.unwrap();
    assert_eq!(rec_f.state, DownloadOperationState::Rejected);
    let own_f = state
        .store()
        .get_download_ownership(dl_id, &rec_f.ownership_tag)
        .unwrap()
        .unwrap();
    assert_eq!(own_f.ownership_state, DownloadOwnershipState::Foreign);
    assert_eq!(
        own_f.provenance_kind,
        DownloadProvenanceKind::ExistingForeign
    );

    dl.set_query_mode(QueryMode::NotFound);
    let ctx_nf = op_ctx("unk-op-nf");
    let op_nf = ctx_nf.operation_id;
    let _ = svc
        .submit(
            default_plan(test_torrent(
                SiteId::new(),
                "nf-1",
                &srv.url("nf.torrent"),
                None,
            )),
            ctx_nf,
        )
        .await
        .unwrap();
    api::reconcile_plugin_pending_for_test(&state).unwrap();
    assert_eq!(
        state
            .store()
            .get_download_operation(&op_nf.to_string())
            .unwrap()
            .unwrap()
            .state,
        DownloadOperationState::Failed
    );
}

fn build_dl_cmd_env(
    cmd_bytes: Vec<u8>,
) -> (
    HttpEnv,
    Arc<RecordingDownloader>,
    StubTorrentServer,
    ApiState,
) {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let dl = Arc::new(RecordingDownloader::new(dir.path().join("stage")));
    let srv_cmd = StubTorrentServer::start(cmd_bytes);
    let state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl.clone(),
    )
    .unwrap()
    .with_plugin_http_policy_override(
        net_guard::TargetPolicy::production().with_test_loopback_socket(srv_cmd.addr()),
    );
    let env = HttpEnv {
        _dir: dir,
        state: state.clone(),
        app: api::router(state.clone()),
        member_token: String::new(),
    };
    (env, dl, srv_cmd, state)
}

#[tokio::test]
async fn production_effect_adapter_executes_and_reconciles_downloader_add() {
    let (cmd_bytes, _) = minimal_torrent_bytes("Plugin.Command.Add", 1024);
    let (env, dl, srv_cmd, state) = build_dl_cmd_env(cmd_bytes);

    dl.set_unknown_mode(UnknownMode::PostSendAccepted);
    env.enable_dev_mode().await;
    let nil_dl = DownloaderId::from_uuid(uuid::Uuid::nil());
    let pkg_bytes = build_policy_pkg(
        "org.example.dl-cmd",
        "[[components.hooks]]\nid = \"download.after\"\npriority = 10\n",
        &[
            "hooks:download.after",
            "events:download.after",
            "command:downloader.add",
        ],
    );
    let inst = env
        .install_pkg(
            &pkg_bytes,
            None,
            json!([
                { "capability": "hooks:download.after", "scope": "Global", "sensitive": false },
                { "capability": "events:download.after", "scope": "Global", "sensitive": false },
                {
                    "capability": "command:downloader.add",
                    "scope": { "Object": { "kind": "downloader", "id": nil_dl.to_string() } },
                    "sensitive": false
                }
            ]),
        )
        .await;
    let rev = inst["revision"].as_i64().unwrap();
    env.enable_plugin("org.example.dl-cmd", rev, "en-dl-cmd")
        .await;
    let plugin = state
        .store()
        .get_plugin("org.example.dl-cmd")
        .unwrap()
        .unwrap();
    let authority = state
        .store()
        .issue_plugin_state_authority(&plugin, None)
        .unwrap();

    let payload = serde_json::to_vec(&json!({
        "downloader_id": nil_dl.to_string(),
        "download_ref": srv_cmd.url("cmd.torrent"),
        "paused": true,
        "tags": []
    }))
    .unwrap();
    let accepted = api::submit_plugin_command_for_test(
        &state,
        api::SubmitPluginCommand {
            role: api::PluginInvocationRole::Task,
            authority,
            subject: PluginSubject::Task {
                plugin_id: plugin.plugin_id.clone(),
                task_id: "add-task".into(),
            },
            kind: "downloader.add".into(),
            targets: vec![store::plugins::PluginCommandTarget::Downloader(nil_dl)],
            idempotency_key: "cmd-dl-add-1".into(),
            request: store::plugins::CanonicalPluginRequest {
                method: "GUEST-CMD".into(),
                path: "/plugins/org.example.dl-cmd/commands/downloader.add".into(),
                body_digest: sha2::Sha256::digest(&payload).into(),
            },
            expected_revision: None,
            cause: PluginCommandCause::root("cause-cmd-add"),
            payload,
        },
    )
    .unwrap();
    let cmd_op_id = PluginOperationId::from_uuid(accepted.operation.operation_id);
    api::run_plugin_command_for_test(&state, cmd_op_id).unwrap();
    assert_eq!(
        state
            .store()
            .get_plugin_operation_admin(cmd_op_id.as_uuid())
            .unwrap()
            .unwrap()
            .operation
            .state,
        PluginOperationState::Unknown
    );
    dl.set_unknown_mode(UnknownMode::Off);
    api::reconcile_plugin_pending_for_test(&state).unwrap();
    api::run_plugin_reconcile_for_test(&state, cmd_op_id).unwrap();
    assert_eq!(
        state
            .store()
            .get_plugin_operation_admin(cmd_op_id.as_uuid())
            .unwrap()
            .unwrap()
            .operation
            .state,
        PluginOperationState::Succeeded
    );
}

#[tokio::test]
async fn download_service_runs_download_before_and_after_hooks() {
    let (torrent_bytes, _) = minimal_torrent_bytes("Hooked.Download", 2048);
    let (env, dl, srv, state) = build_dl_cmd_env(torrent_bytes);
    env.enable_dev_mode().await;
    let pkg_bytes = build_policy_pkg(
        "org.example.dl-hooks",
        "[[components.hooks]]\nid = \"download.before\"\npriority = 10\n\n[[components.hooks]]\nid = \"download.after\"\npriority = 10\n",
        &[
            "hooks:download.before",
            "hooks:download.after",
            "events:download.after",
        ],
    );
    let inst = env
        .install_pkg(
            &pkg_bytes,
            None,
            json!([
                { "capability": "hooks:download.before", "scope": "Global", "sensitive": false },
                { "capability": "hooks:download.after", "scope": "Global", "sensitive": false },
                { "capability": "events:download.after", "scope": "Global", "sensitive": false }
            ]),
        )
        .await;
    let rev = inst["revision"].as_i64().unwrap();
    env.enable_plugin("org.example.dl-hooks", rev, "en-dl-hooks")
        .await;

    let svc = ApiDownloadService::new(state);
    let ok_torrent = test_torrent(SiteId::new(), "ok-1", &srv.url("ok.torrent"), None);
    let out_ok = svc
        .submit(default_plan(ok_torrent), op_ctx("hook-ok-1"))
        .await
        .unwrap();
    assert!(matches!(out_ok, DownloadOutcome::Submitted { .. }));
    assert_eq!(dl.submit_count.load(Ordering::SeqCst), 1);
    let submitted = dl.inner.submitted();
    assert_eq!(submitted.len(), 1);
    assert_eq!(
        submitted[0].options.save_path.as_deref(),
        Some("plugins/demo"),
        "download.before filter patch must be applied before submit_verified"
    );
}
