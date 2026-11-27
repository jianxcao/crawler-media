#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;
#[allow(dead_code)]
#[path = "subscribe_hooks_and_admit/support.rs"]
mod support;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use api::plugins::reconcile_pending;
use api::{
    ApiDownloadService, ApiState, DownloadOpContext, DownloadOutcome, FrozenDownloadPlan, Store,
    SubmissionPurpose, TorrentSource, router,
};
use domain::{PluginOperationId, Site, SiteId, Torrent};
use downloader::{
    ClientSubmitResult, ClientTaskQuery, DiskSpaceInfo, Downloader, DownloaderError,
    MemoryDownloader, SubmitOptions, SubmitPayload, TaskFileSet, TaskSnapshot, torrent_info_hash,
};
use indexer::RegistrySnapshot;
use plugin_http_support::{ADMIN_PASS, HttpEnv, TOKEN};
use store::BoostTaskState;
use support::{StubSearchFetcher, StubTorrentServer, minimal_torrent_bytes};

struct UnknownAfterAddDownloader {
    inner: Arc<MemoryDownloader>,
    force_unknown: AtomicBool,
}

impl Downloader for UnknownAfterAddDownloader {
    fn remember_identity(&self, t: &Torrent, h: &str) -> Result<(), DownloaderError> {
        self.inner.remember_identity(t, h)
    }

    fn submit_verified(
        &self,
        payload: &SubmitPayload,
        options: &SubmitOptions,
    ) -> Result<ClientSubmitResult, DownloaderError> {
        let res = self.inner.submit_verified(payload, options)?;
        if self.force_unknown.load(Ordering::SeqCst) {
            return Ok(ClientSubmitResult::UnknownOutcome {
                reason: "post-send timeout".into(),
            });
        }
        Ok(res)
    }

    fn query_task_status(
        &self,
        info_hash: &str,
        ownership_tag: &str,
    ) -> Result<ClientTaskQuery, DownloaderError> {
        self.inner.query_task_status(info_hash, ownership_tag)
    }

    fn completed_files(&self, t: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        self.inner.completed_files(t)
    }

    fn delete_task(&self, info_hash: &str, delete_files: bool) -> Result<(), DownloaderError> {
        self.inner.delete_task(info_hash, delete_files)
    }

    fn pause_task(&self, h: &str) -> Result<(), DownloaderError> {
        self.inner.pause_task(h)
    }

    fn resume_task(&self, h: &str) -> Result<(), DownloaderError> {
        self.inner.resume_task(h)
    }

    fn task_snapshots(&self) -> Result<Vec<TaskSnapshot>, DownloaderError> {
        self.inner.task_snapshots()
    }

    fn task_files_by_hash(&self, info_hash: &str) -> Result<Option<TaskFileSet>, DownloaderError> {
        self.inner.task_files_by_hash(info_hash)
    }

    fn disk_free_space(&self) -> Result<DiskSpaceInfo, DownloaderError> {
        self.inner.disk_free_space()
    }
}

fn make_env(dir: &tempfile::TempDir, dl: Arc<dyn Downloader>, addrs: &[SocketAddr]) -> HttpEnv {
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let snap = RegistrySnapshot::builtin().unwrap();
    let fetcher = Arc::new(StubSearchFetcher {
        payload: String::new(),
    });
    let mut state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        snap,
        fetcher,
        dl,
    )
    .unwrap();
    if !addrs.is_empty() {
        let pol = addrs
            .iter()
            .fold(net_guard::TargetPolicy::production(), |p, a| {
                p.with_test_loopback_socket(*a)
            });
        state = state.with_plugin_http_policy_override(pol);
    }
    HttpEnv {
        _dir: tempfile::tempdir_in("/tmp").unwrap(),
        app: router(state.clone()),
        state,
        member_token: String::new(),
    }
}

fn seed_site(env: &HttpEnv, name: &str, url: &str) -> SiteId {
    let id = SiteId::new();
    env.state
        .store()
        .insert_site(&Site {
            id,
            name: name.into(),
            url: url.into(),
            profile_id: "demo".into(),
            cookie: Some("uid=1; pass=init".into()),
            api_key: None,
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: None,
            cdp_url: None,
            downloader_id: None,
            enabled: true,
            maintenance_via: domain::MaintenanceVia::Http,
            browser_search: false,
            timezone: "UTC".into(),
        })
        .unwrap();
    id
}

fn build_plan_and_ctx(
    site_id: SiteId,
    url: &str,
    title: &str,
    purpose: SubmissionPurpose,
    plugin_id: Option<&str>,
    idem_key: &str,
) -> (FrozenDownloadPlan, DownloadOpContext) {
    let torrent = Torrent {
        site_id,
        id: Some("1".into()),
        title: title.into(),
        enclosure: url.into(),
        size_bytes: Some(4096),
        seeders: Some(10),
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
    let plan = FrozenDownloadPlan {
        torrent,
        source: TorrentSource::from_url(url).unwrap(),
        purpose,
        downloader_id: None,
        frozen_endpoint: None,
        subscribe_id: None,
        subscribe_generation: None,
        policy_revision: None,
        media_id: None,
        score: Some(80),
        release_override: None,
        force: false,
        save_path: None,
        category: None,
        tags: vec![],
        paused: false,
        upload_limit: None,
        download_limit: None,
        sequential: false,
        first_last_piece: false,
        budget_key: None,
        plan_digest: None,
    };
    let op_id = PluginOperationId::new();
    let ctx = DownloadOpContext {
        operation_id: op_id,
        cause_id: format!("r7:{op_id}"),
        subject: "user:r7".into(),
        idempotency_key: idem_key.into(),
        plugin_id: plugin_id.map(str::to_string),
        generation: None,
        config_revision: None,
        dry_run: false,
    };
    (plan, ctx)
}

#[test]
fn download_service_and_set_task_tags_reject_reserved_system_tags_and_unsafe_plugin_paths() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let b1 = minimal_torrent_bytes("TagGuard.One.2026.1080p", 4096);
    let hash = torrent_info_hash(&b1).unwrap();
    let srv = StubTorrentServer::start(b1);
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let env = make_env(&dir, dl.clone(), &[srv.addr]);
    let site_id = seed_site(&env, "TagGuardSite", &srv.url(""));
    let svc = ApiDownloadService::new(env.state.clone());

    for (idx, bad_tag) in [
        "crawler-media-claimed",
        "crawler-media-owned-deadbeef",
        "crawler-media",
        "cm-op:victim-op",
        "cm-sub:victim-sub",
        "cm:plugin:other:1",
        "cm_internal",
        "plugin:p:a,b",
        "plugin:p:a\nb",
    ]
    .into_iter()
    .enumerate()
    {
        let (mut plan, ctx) = build_plan_and_ctx(
            site_id,
            &srv.url(&format!("bad-tag-{idx}.torrent")),
            "TagGuard.One.2026.1080p",
            SubmissionPurpose::Boost,
            Some("cm.boost"),
            &format!("bad-tag-{idx}"),
        );
        plan.tags = vec![bad_tag.into()];
        assert!(
            svc.submit_sync(plan, ctx).is_err(),
            "expected submit_sync rejection for reserved/invalid tag `{bad_tag}`"
        );
    }

    for (idx, bad_dir) in ["../escape", "/abs/path", "a\\b", "bad\ndir"]
        .into_iter()
        .enumerate()
    {
        let (mut plan, ctx) = build_plan_and_ctx(
            site_id,
            &srv.url(&format!("bad-dir-{idx}.torrent")),
            "TagGuard.One.2026.1080p",
            SubmissionPurpose::Plugin,
            Some("org.example.p"),
            &format!("bad-dir-{idx}"),
        );
        plan.save_path = Some(bad_dir.into());
        assert!(
            svc.submit_sync(plan, ctx).is_err(),
            "expected submit_sync rejection for unsafe relative_directory `{bad_dir}`"
        );
    }

    let (mut bad_cat, bad_cat_ctx) = build_plan_and_ctx(
        site_id,
        &srv.url("bad-cat.torrent"),
        "TagGuard.One.2026.1080p",
        SubmissionPurpose::Boost,
        Some("cm.boost"),
        "bad-cat",
    );
    bad_cat.category = Some("bad\ncat".into());
    assert!(svc.submit_sync(bad_cat, bad_cat_ctx).is_err());
    assert!(dl.submitted().is_empty());

    // Valid plugin-namespaced tag succeeds; set_task_tags then rejects unowned cm:/cm-/cm_ prefixes.
    let (mut ok_plan, ok_ctx) = build_plan_and_ctx(
        site_id,
        &srv.url("ok.torrent"),
        "TagGuard.One.2026.1080p",
        SubmissionPurpose::Boost,
        Some("cm.boost"),
        "ok-plan",
    );
    ok_plan.tags = vec!["plugin:cm.boost:slot1".into()];
    assert!(matches!(
        svc.submit_sync(ok_plan, ok_ctx).unwrap(),
        DownloadOutcome::Submitted { .. }
    ));
    for reserved in [
        "cm:plugin:other:1",
        "cm-claimed",
        "cm_internal",
        "crawler-media-x",
    ] {
        assert!(
            dl.set_task_tags(&hash, &[reserved.to_string()]).is_err(),
            "set_task_tags must reject unowned reserved tag `{reserved}`"
        );
    }
}

#[test]
fn boost_unknown_outcome_reconcile_preserves_paused_state() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let bytes = minimal_torrent_bytes("BoostUnknownPaused.2026.1080p", 4096);
    let hash = torrent_info_hash(&bytes).unwrap();
    let srv = StubTorrentServer::start(bytes);
    let mem = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let dl = Arc::new(UnknownAfterAddDownloader {
        inner: mem.clone(),
        force_unknown: AtomicBool::new(true),
    });
    let env = make_env(&dir, dl, &[srv.addr]);
    let site_id = seed_site(&env, "BoostUnknownSite", &srv.url(""));
    let svc = ApiDownloadService::new(env.state.clone());

    let (mut plan, ctx) = build_plan_and_ctx(
        site_id,
        &srv.url("unk-paused.torrent"),
        "BoostUnknownPaused.2026.1080p",
        SubmissionPurpose::Boost,
        Some("cm.boost"),
        "unk-paused-1",
    );
    plan.paused = true;
    plan.budget_key = Some("cm.boost:default".into());

    let out = svc.submit_sync(plan, ctx).unwrap();
    assert!(matches!(out, DownloadOutcome::Unknown { .. }), "{out:?}");
    assert!(mem.is_task_paused(&hash));

    reconcile_pending(&env.state, store::unix_now() + 5).unwrap();

    let facts = env.state.store().list_active_boost_task_facts().unwrap();
    let fact = facts.iter().find(|f| f.info_hash == hash).unwrap();
    assert_eq!(
        fact.state,
        BoostTaskState::Paused,
        "reconciled paused Boost task must record BoostTaskState::Paused"
    );
}
