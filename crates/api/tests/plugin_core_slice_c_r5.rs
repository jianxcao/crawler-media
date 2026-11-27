#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;
#[allow(dead_code)]
#[path = "subscribe_hooks_and_admit/support.rs"]
mod support;

use std::path::PathBuf;
use std::sync::Arc;

use api::{
    ApiDownloadService, ApiState, DownloadOpContext, DownloadOutcome, FrozenDownloadPlan, Store,
    SubmissionPurpose, TorrentSource, router,
};
use axum::http::StatusCode;
use base64::Engine as _;
use domain::{DownloaderId, PluginOperationId, Site, SiteId, SubscribeId, Torrent};
use downloader::{
    ClientSubmitResult, Downloader, DownloaderError, MemoryDownloader, SubmitOptions,
    SubmitPayload, TaskSnapshot, torrent_info_hash,
};
use indexer::RegistrySnapshot;
use jobs::Runner as _;
use plugin_api::HOOK_WORLD;
use plugin_http_support::{ADMIN_PASS, HttpEnv, TOKEN, assert_err, assert_ok, pkg, req_json};
use serde_json::{Value, json};
use store::DownloadOwnershipState;
use support::{
    StubSearchFetcher, StubTorrentServer, enable_hooks_plugin, minimal_torrent_bytes, nexus_row,
};
use uuid::Uuid;

const SUB_SELECT_DEFER_B64: &str = "AGFzbQ0AAQAH3RcBQnoBbQ4RcGVybWlzc2lvbi1kZW5pZWQNd3JvbmctY29udGV4dAlub3QtZm91bmQQaW52YWxpZC1hcmd1bWVudAxzdGFsZS1oYW5kbGUQc3RhbGUtZ2VuZXJhdGlvbghjb25mbGljdA5xdW90YS1leGNlZWRlZARidXN5EWRlYWRsaW5lLWV4Y2VlZGVkCWNhbmNlbGxlZAt1bnN1cHBvcnRlZAh1cHN0cmVhbQ91bmtub3duLW91dGNvbWUEAAplcnJvci1jb2RlAwAAAW0FDGF1dGgtZXhwaXJlZAxyYXRlLWxpbWl0ZWQJY2hhbGxlbmdlB25ldHdvcmsFcGFyc2UEAA11cHN0cmVhbS1raW5kAwACAXEFCHVwc3RyZWFtAQMAD2Jsb2NrZWQtYWRkcmVzcwAADmJvZHktdG9vLWxhcmdlAAAdYnJvd3Nlci1pc29sYXRpb24tdW5hdmFpbGFibGUAABF1bnN1cHBvcnRlZC1yb3V0ZQAABAAMZXJyb3ItZGV0YWlsAwAEAWsFAWt3AWtzAXIFBGNvZGUBB21lc3NhZ2VzBmRldGFpbAYOcmV0cnktYWZ0ZXItbXMHDG9wZXJhdGlvbi1pZAgEAAlhcGktZXJyb3IDAAkBa3MBa3MBa3MBcgsNaW52b2NhdGlvbi1pZHMIY2F1c2UtaWRzCXBsdWdpbi1pZHMKZ2VuZXJhdGlvbncPY29uZmlnLXJldmlzaW9udwdzdWJqZWN0cwdzaXRlLWlkCwxzdWJzY3JpYmUtaWQMCmxpYnJhcnktaWQNC2RlYWRsaW5lLW1zdwljYW5jZWxsZWR/BAAHY29udGV4dAMADgFrdwFreQFreQFrdwFrfwFrfwFrcwFregFwcwFyDQZoYW5kbGVzB3NpdGUtaWRzBXRpdGxlcwRzaXplEAdzZWVkZXJzEQhsZWVjaGVycxIMcHVibGlzaGVkLWF0EwRmcmVlFAJochUHcmVsZWFzZRYFc2NvcmUXBmxhYmVscxgIcmV2aXNpb253BAAMdG9ycmVudC12aWV3AwAZAXICB3NpdGUtaWRzBWVycm9yCgQADHNpdGUtZmFpbHVyZQMAGwFwGgFwHAFyAgpjYW5kaWRhdGVzHQ1zaXRlLWZhaWx1cmVzHgQADHNlYXJjaC1hZnRlcgMAHwFwcwFyAwdrZXl3b3JkcwRwYWdleQpjYXRlZ29yaWVzIQQADHNlYXJjaC1xdWVyeQMAIgFwcwFyAgVxdWVyeSMIc2l0ZS1pZHMkBAANc2VhcmNoLWJlZm9yZQMAJQFwGgFyAwdzaXRlLWlkcwVxdWVyeSMKY2FuZGlkYXRlcycEABFzZWFyY2gtc2l0ZS1hZnRlcgMAKAFwGgFwcwFyAgpjYW5kaWRhdGVzKhBhZG1pdHRlZC1oYW5kbGVzKwQADGZpbHRlci1hZnRlcgMALAFwGgFyAwxzdWJzY3JpYmUtaWRzCG1lZGlhLWlkcwpjYW5kaWRhdGVzLgQAD3N1YnNjcmliZS1zY29yZQMALwFwGgFwcwFyBAxzdWJzY3JpYmUtaWRzCG1lZGlhLWlkcwpjYW5kaWRhdGVzMRBhZG1pdHRlZC1oYW5kbGVzMgQAEHN1YnNjcmliZS1zZWxlY3QDADMBa3MBa3MBcHMBa3cBa3cBcgkNZG93bmxvYWRlci1pZHMScmVsYXRpdmUtZGlyZWN0b3J5NQhjYXRlZ29yeTYEdGFnczcGcGF1c2Vkfwx1cGxvYWQtbGltaXQ4DmRvd25sb2FkLWxpbWl0OQpzZXF1ZW50aWFsfxBmaXJzdC1sYXN0LXBpZWNlfwQAEmRvd25sb2FkLXBsYW4tdmlldwMAOgFyAwljYW5kaWRhdGUaB3B1cnBvc2VzBHBsYW47BAAPZG93bmxvYWQtYmVmb3JlAwA8AXIFCG1lZGlhLWlkcwpsaWJyYXJ5LWlkcwtzb3VyY2UtcGF0aHMWcHJvcG9zZWQtcmVsYXRpdmUtcGF0aHMIaXMtbW92aWV/BAAPdHJhbnNmZXItcmVuYW1lAwA+AXEIDXNlYXJjaC1iZWZvcmUBJgARc2VhcmNoLXNpdGUtYWZ0ZXIBKQAMc2VhcmNoLWFmdGVyASAADGZpbHRlci1hZnRlcgEtAA9zdWJzY3JpYmUtc2NvcmUBMAAQc3Vic2NyaWJlLXNlbGVjdAE0AA9kb3dubG9hZC1iZWZvcmUBPQAPdHJhbnNmZXItcmVuYW1lAT8ABAAOZmlsdGVyLXBheWxvYWQDAEABcgMOc2NoZW1hLXZlcnNpb255B2NvbnRleHQPB3BheWxvYWTBAAQADGZpbHRlci1pbnB1dAMAQgFyAwZoYW5kbGVzBWRlbHRhegZyZWFzb25zBAANcmVzY29yZS1wYXRjaAMARAFrcwFwcwFrxwABcHMBa8kAAXIDB2tleXdvcmTGAAhzaXRlLWlkc8gACmNhdGVnb3JpZXPKAAQAEnNlYXJjaC1xdWVyeS1wYXRjaAMASwFrcwFrcwFrcwFwcwFwcwFrfwFrdwFrdwFrfwFrfwFyCg1kb3dubG9hZGVyLWlkzQAScmVsYXRpdmUtZGlyZWN0b3J5zgAIY2F0ZWdvcnnPAAhhZGQtdGFnc9AAC3JlbW92ZS10YWdz0QAGcGF1c2Vk0gAMdXBsb2FkLWxpbWl00wAOZG93bmxvYWQtbGltaXTUAApzZXF1ZW50aWFs1QAQZmlyc3QtbGFzdC1waWVjZdYABAATZG93bmxvYWQtcGxhbi1wYXRjaAMAVwFwcwFwcwFxBwRkcm9wAXMACXNldC1vcmRlcgHZAAAHcmVzY29yZQHFAAAMc2VhcmNoLXF1ZXJ5AcwAAA5zZWxlY3QtaGFuZGxlcwHaAAANZG93bmxvYWQtcGxhbgHYAAALcmVuYW1lLXBhdGgBcwAEAAVwYXRjaAMAWwFxBARub25lAAANc2VhcmNoLWJlZm9yZQEmABBzdWJzY3JpYmUtc2VsZWN0ATQAD2Rvd25sb2FkLWJlZm9yZQE9AAQADGdhdGUtcGF5bG9hZAMAXQFwcwFyBg5zY2hlbWEtdmVyc2lvbnkHY29udGV4dA8HaG9vay1pZHMHaGFuZGxlc98ACHJldmlzaW9udwdwYXlsb2Fk3gAEAApnYXRlLWlucHV0AwBgAW8Cd3MBcQQFYWxsb3cAAAdhYnN0YWluAAAGcmVqZWN0AXMABWRlZmVyAeIAAAQADWdhdGUtZGVjaXNpb24DAGMBa3MBa3MBa3cBcgUMb3BlcmF0aW9uLWlkcw1kb3dubG9hZGVyLWlk5QAJaW5mby1oYXNo5gAHb3V0Y29tZXMUdmVyaWZpZWQtdG90YWwtYnl0ZXPnAAQAFGRvd25sb2FkLWFmdGVyLWV2ZW50AwBoAXIFCG1lZGlhLWlkcwpsaWJyYXJ5LWlkcw1yZWxhdGl2ZS1wYXRocwpmaWxlLWNvdW50eQt0b3RhbC1ieXRlc3cEABh0cmFuc2Zlci1jb21wbGV0ZWQtZXZlbnQDAGoBcgQHc2l0ZS1pZHMGYWN0aW9ucwVzdGF0ZXMKd2luZG93LWtleXMEABhzaXRlLWNoZWNraW4tYWZ0ZXItZXZlbnQDAGwBcQQEbm9uZQAADmRvd25sb2FkLWFmdGVyAekAABJ0cmFuc2Zlci1jb21wbGV0ZWQB6wAAEnNpdGUtY2hlY2tpbi1hZnRlcgHtAAAEAA1ldmVudC1wYXlsb2FkAwBuAXBzAXIIDnNjaGVtYS12ZXJzaW9ueQdjb250ZXh0DwJpZHMEa2luZHMLb2NjdXJyZWQtYXR3B2hhbmRsZXPwAAhyZXZpc2lvbncHcGF5bG9hZO8ABAAFZXZlbnQDAHEBcgMGaGFuZGxlcwhyZXZpc2lvbncLb2JzZXJ2ZWQtYXR3BAALZW50aXR5LXZpZXcDAHMBcHMBa3kBa3MBcgQEa2luZHMHYWxpYXNlc/UABHllYXL2AAtleHRlcm5hbC1pZPcABAAPbWVkaWEtcmVmZXJlbmNlAwB4Ch8BABpjcmF3bGVyOnBsdWdpbi90eXBlc0AxLjAuMAUABnEIAwAACWFwaS1lcnJvcgMAAAtlbnRpdHktdmlldwMAAA9tZWRpYS1yZWZlcmVuY2UDAAAMZmlsdGVyLWlucHV0AwAABXBhdGNoAwAACmdhdGUtaW5wdXQDAAANZ2F0ZS1kZWNpc2lvbgMAAAVldmVudAHEAwBhc20BAAAAARkEYAR/f39/AX9gA39/fwF/YAABf2ABfwF/AwcGAAECAwMDBQMBAAIGCAF/AUGAgAELBz4HA21lbQIAB3JlYWxsb2MAAAh2YWxpZGF0ZQABBmhlYWx0aAACBmZpbHRlcgADBGdhdGUABAZub3RpZnkABQrlAQYgAQF/IwAgAkEBa2ogAkEBa0F/c3EhBCAEIANqJAAgBAsQAEGAIEEAQcAA/AsAQYAgCxAAQYAgQQBBwAD8CwBBgCALUwBBgCBBAEGACPwLAEGAJkGABDYCAEGEJkEBNgIAQYAkQQQ6AABBiCRBgCY2AgBBjCRBATYCAEGAIEEAOgAAQYggQYAkNgIAQYwgQQE2AgBBgCALOwBBgCBBAEHAAPwLAEGAIEEAOgAAQYggQQM6AABBkCBC4NQDNwMAQZggQZAENgIAQZwgQRU2AgBBgCALEABBgCBBAEHAAPwLAEGAIAsLIwIAQYAECwEwAEGQBAsVc2VsZWN0LWRlZmVyLWNvb2xkb3duADoEbmFtZQACAW0CJgEABQADb2xkAQdvbGRfbGVuAgVhbGlnbgMHbmV3X2xlbgQDcHRyBwcBAARidW1wAgQBAAAABhUCAAIBAANtZW0AAAEAB3JlYWxsb2MHGQFxAgdoZWFsdGh5AAAIZGVncmFkZWQBcwAGGQIAAAEACHZhbGlkYXRlAAABAAZoZWFsdGgHJgJqAAEBQAINY29uZmlnLXNjaGVtYXMMc3RhdGUtc2NoZW1heQAKCAsBAAABAwMABAAACwcKAmoBCQEBQAAADAgJAQAAAgIDAAANBTYBAQQACWFwaS1lcnJvcgMBAAxoZWFsdGgtc3RhdGUDCQAIdmFsaWRhdGUBAAAGaGVhbHRoAQELJAEAHmNyYXdsZXI6cGx1Z2luL2xpZmVjeWNsZUAxLjAuMAUBAAYgAwAAAQAGZmlsdGVyAAABAARnYXRlAAABAAZub3RpZnkHEwNwBWoBDgEBQAEFaW5wdXQEAA8ICwEAAAMDAwAEAAAQBxECagEHAQFAAQVpbnB1dAYAEQgLAQAABAMDAAQAABIHEAJqAAEBQAEFZXZlbnQIABMICwEAAAUDAwAEAAAUBW0BAQkACWFwaS1lcnJvcgMBAAxmaWx0ZXItaW5wdXQDBAAFcGF0Y2gDBQAKZ2F0ZS1pbnB1dAMGAA1nYXRlLWRlY2lzaW9uAwcABWV2ZW50AwgABmZpbHRlcgECAARnYXRlAQMABm5vdGlmeQEECyABABpjcmF3bGVyOnBsdWdpbi9ob29rc0AxLjAuMAUDAACkAg5jb21wb25lbnQtbmFtZQExAAAGAAdyZWFsbG9jAQVjX3ZhbAIGY19obHRoAwVjX2ZsdAQGY19nYXRlBQVjX250ZgEIAAIBAANtZW0BBgARAQABbQEJABIBAARjb3JlAScBBQAFbF92YWwBBmxfaGx0aAIFaF9mbHQDBmhfZ2F0ZQQFaF9udGYBcwMKAAt0eS1pbnN0YW5jZQEJYXBpLWVycm9yAgtlbnRpdHktdmlldwMPbWVkaWEtcmVmZXJlbmNlBAxmaWx0ZXItaW5wdXQFBXBhdGNoBgpnYXRlLWlucHV0Bw1nYXRlLWRlY2lzaW9uCAVldmVudAkCaHMBJQUDAAV0eXBlcwEObGlmZWN5Y2xlX2luc3QDCmhvb2tzX2luc3Q=";

fn make_env(
    dir: &tempfile::TempDir,
    payload: &str,
    dl: Arc<dyn Downloader>,
    addrs: &[std::net::SocketAddr],
) -> HttpEnv {
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let snap = RegistrySnapshot::builtin().unwrap();
    let fetcher = Arc::new(StubSearchFetcher {
        payload: payload.into(),
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

fn seed_site(env: &HttpEnv, name: &str, url: &str, profile: &str) -> SiteId {
    let id = SiteId::new();
    env.state
        .store()
        .insert_site(&Site {
            id,
            name: name.into(),
            url: url.into(),
            profile_id: profile.into(),
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

async fn post_json(
    env: &HttpEnv,
    uri: &str,
    hdrs: &[(&str, &str)],
    body: Value,
) -> (StatusCode, Value) {
    let (st, _, b) = env
        .send(req_json("POST", uri, Some(TOKEN), hdrs, Some(body)))
        .await;
    (st, b)
}

async fn req_authed(env: &HttpEnv, method: &str, uri: &str) -> (StatusCode, Value) {
    let (st, _, b) = env
        .send(req_json(method, uri, Some(TOKEN), &[], None))
        .await;
    (st, b)
}

async fn create_tv_sub(env: &HttpEnv, title: &str) -> SubscribeId {
    let cov = json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2 });
    let media = json!({ "kind": "tv", "title": title, "original_title": title });
    let body = json!({
        "media": media,
        "coverage": cov,
        "fetch_mode": "search",
        "follow_future": false
    });
    let (st, b) = post_json(env, "/api/v1/subscriptions", &[], body).await;
    assert_ok(st, &b, StatusCode::CREATED)["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

fn build_plan_and_ctx(
    (site_id, url, title): (SiteId, &str, &str),
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
        cause_id: format!("r5:{op_id}"),
        subject: "user:r5".into(),
        idempotency_key: idem_key.into(),
        plugin_id: plugin_id.map(str::to_string),
        generation: None,
        config_revision: None,
        dry_run: false,
    };
    (plan, ctx)
}

async fn install_custom_hook_wasm(env: &HttpEnv, id: &str, hook_id: &str, wasm_bytes: &[u8]) {
    env.enable_dev_mode().await;
    let cap = format!("hooks:{hook_id}");
    let schema = pkg::sample_schema(false, None);
    let manifest = format!(
        r#"manifest_version = 1
id = "{id}"
name = "Custom {id}"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["test"]
description = "Custom hook fixture"
config_schema = "config.schema.json"
state_schema = 1

[[components]]
id = "policy"
entry = "policy.wasm"
world = "{HOOK_WORLD}"
permissions = ["{cap}"]

[[components.hooks]]
id = "{hook_id}"
priority = 10
"#
    );
    let zip = pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", schema.as_bytes()),
        ("policy.wasm", wasm_bytes),
    ]);
    let grants = json!([{ "capability": cap, "scope": "Global", "sensitive": false }]);
    let inst = env.install_pkg(&zip, None, grants).await;
    env.enable_plugin(id, inst["revision"].as_i64().unwrap(), &format!("en-{id}"))
        .await;
}

async fn enqueue_and_run_search(env: &HttpEnv, sub_id: SubscribeId) -> Result<(), String> {
    let job = {
        let jobs = env.state.jobs();
        let q = jobs.acquire().unwrap();
        let payload = json!({ "subscribe_id": sub_id.to_string() }).to_string();
        q.enqueue(jobs::JobIdentity::subscribe_search(sub_id), &payload, 0)
            .unwrap();
        q.claim(store::unix_now() + 3600, 10)
            .unwrap()
            .into_iter()
            .find(|j| j.scope == jobs::JobScope::Subscribe(sub_id))
            .unwrap()
    };
    let st = env.state.clone();
    tokio::task::spawn_blocking(move || api::worker::StateRunner { state: st }.run(&job))
        .await
        .unwrap()
}

// --- Item 1: crates/api/src/lib.rs purity ---

#[tokio::test]
async fn api_lib_rs_contains_zero_fn_declarations_and_test_reexports_work() {
    let lib_src =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs")).unwrap();
    assert!(
        !lib_src.lines().any(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with("//") && (trimmed.contains("fn ") || trimmed.starts_with("fn("))
        }),
        "crates/api/src/lib.rs must contain only module tree and re-exports"
    );
    let env = HttpEnv::new().await;
    api::check_in_run_for_test(&env.state).unwrap();
    api::watch_intake_run_for_test(&env.state).unwrap();
}

// --- Item 3: subscribe.select Gate::Defer post-filter p_admitted & persistence ---

#[tokio::test]
async fn subscribe_select_defer_persists_suppresses_cooldown_surfaces_activities_and_skips_dropped()
{
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let t1 = minimal_torrent_bytes("SelectShow.S01E01.1080p.WEB-DL", 4096);
    let t2 = minimal_torrent_bytes("SelectShow.S01E02.1080p.WEB-DL", 4096);
    let (srv1, srv2) = (StubTorrentServer::start(t1), StubTorrentServer::start(t2));
    let (enc1, enc2) = (srv1.url("s-e01.torrent"), srv2.url("s-e02.torrent"));
    let html = format!(
        r#"<!DOCTYPE html><html><body><table class="torrents">
        <tr class="torrent"><td><a class="name" title="SelectShow.S01E01.1080p.WEB-DL" href="details.php?id=1">x</a></td><td><a class="download" href="{enc1}"></a></td><td class="size">4.00 GB</td><td class="seeders">42</td><td class="leechers">7</td><td class="snatched">180</td><td class="upload_time">2024-01-15 08:30</td><td class="category">TV</td></tr>
        <tr class="torrent"><td><a class="name" title="SelectShow.S01E02.1080p.WEB-DL" href="details.php?id=2">y</a></td><td><a class="download" href="{enc2}"></a></td><td class="size">4.00 GB</td><td class="seeders">40</td><td class="leechers">5</td><td class="snatched">120</td><td class="upload_time">2024-01-15 09:00</td><td class="category">TV</td></tr>
        </table></body></html>"#
    );
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let env = make_env(&dir, &html, dl.clone(), &[srv1.addr, srv2.addr]);
    env.state
        .store()
        .put_setting(api::settings_keys::SUBSCRIBE_AIR_DATE_GATE, "false")
        .unwrap();
    seed_site(&env, "SelectSite", &srv1.url(""), "demo");
    let wasm = base64::engine::general_purpose::STANDARD
        .decode(SUB_SELECT_DEFER_B64)
        .unwrap();
    install_custom_hook_wasm(&env, "org.example.sub-select-r5", "subscribe.select", &wasm).await;

    // 1) Scheduled worker search: only handle "0" (enc1) is deferred; filtered-out "1" (enc2) is NOT deferred.
    let sub_sched = create_tv_sub(&env, "SelectShow").await;
    enqueue_and_run_search(&env, sub_sched).await.unwrap();
    assert!(dl.submitted().is_empty());
    let defers1 = env
        .state
        .store()
        .list_subscribe_defers(sub_sched, 10)
        .unwrap();
    assert_eq!(
        defers1.len(),
        1,
        "only post-filter handle 0 must be deferred"
    );
    assert_eq!(defers1[0].enclosure, enc1);
    assert_eq!(defers1[0].hook_id, "subscribe.select");
    assert_eq!(
        defers1[0].plugin_id.as_deref(),
        Some("org.example.sub-select-r5")
    );
    assert_eq!(defers1[0].attempt_count, 1);

    // 2) Cooldown suppression on next round: enc1 is suppressed during until_ms, so enc2 becomes handle "0" and is deferred; enc1 attempt_count stays 1!
    let (st_run, b_run) = post_json(
        &env,
        &format!("/api/v1/subscriptions/{sub_sched}/run"),
        &[],
        json!({}),
    )
    .await;
    assert_ok(st_run, &b_run, StatusCode::OK);
    let defers2 = env
        .state
        .store()
        .list_subscribe_defers(sub_sched, 10)
        .unwrap();
    assert_eq!(defers2.len(), 2);
    let d_enc1 = defers2.iter().find(|d| d.enclosure == enc1).unwrap();
    let d_enc2 = defers2.iter().find(|d| d.enclosure == enc2).unwrap();
    assert_eq!(
        d_enc1.attempt_count, 1,
        "enc1 must be suppressed during cooldown rather than re-evaluated"
    );
    assert_eq!(d_enc2.attempt_count, 1);

    // 3) Activities endpoint surfaces deferred_candidate from subscribe.select.
    let (st_act, b_act) = req_authed(
        &env,
        "GET",
        &format!("/api/v1/subscriptions/{sub_sched}/activities"),
    )
    .await;
    let items = assert_ok(st_act, &b_act, StatusCode::OK)["items"]
        .as_array()
        .unwrap()
        .clone();
    assert!(items.iter().any(|i| {
        i["type"].as_str() == Some("deferred_candidate") && i["status"].as_str() == Some("deferred")
    }));
}

#[tokio::test]
async fn download_before_defer_is_not_double_counted_by_subscribe_persistence() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let t_bytes = minimal_torrent_bytes("DeferShow.DeferDownload.S01E01.1080p", 4096);
    let srv = StubTorrentServer::start(t_bytes);
    let enc = srv.url("db-defer.torrent");
    let html = nexus_row("DeferShow.DeferDownload.S01E01.1080p", &enc);
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let env = make_env(&dir, &html, dl, &[srv.addr]);
    env.state
        .store()
        .put_setting(api::settings_keys::SUBSCRIBE_AIR_DATE_GATE, "false")
        .unwrap();
    seed_site(&env, "DbDeferSite", &srv.url(""), "demo");
    enable_hooks_plugin(&env, "org.example.db-defer-r5", &["download.before"]).await;

    let sub_id = create_tv_sub(&env, "DeferShow").await;
    let (st, b) = post_json(
        &env,
        &format!("/api/v1/subscriptions/{sub_id}/run"),
        &[],
        json!({}),
    )
    .await;
    assert_ok(st, &b, StatusCode::OK);
    let defers = env.state.store().list_subscribe_defers(sub_id, 10).unwrap();
    assert_eq!(defers.len(), 1);
    assert_eq!(defers[0].hook_id, "download.before");
    assert_eq!(
        defers[0].attempt_count, 1,
        "download.before defer must not be double-counted"
    );
}

// --- Item 4: Piggybacked Idempotency-Key tombstones, boost_task_facts cleanup, stored_hash fallback ---

#[tokio::test]
async fn delete_task_tombstones_piggybacked_idem_keys_and_cleans_boost_facts() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let t1 = minimal_torrent_bytes("Piggy.Show.S01E01.1080p.WEB-DL", 4096);
    let h1 = torrent_info_hash(&t1).unwrap();
    let srv1 = StubTorrentServer::start(t1);
    let enc1 = srv1.url("piggy1.torrent");
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let env = make_env(&dir, "", dl.clone(), &[srv1.addr]);
    let site1 = seed_site(&env, "PiggySite1", &srv1.url(""), "demo");
    let svc = ApiDownloadService::new(env.state.clone());

    let sub_b1 = json!({
        "download_url": enc1,
        "title": "Piggy.Show.S01E01.1080p.WEB-DL",
        "site_id": site1.to_string(),
    });
    let (s1, b1) = post_json(
        &env,
        "/api/v1/downloaders/submit",
        &[("idempotency-key", "pig-m1")],
        sub_b1.clone(),
    )
    .await;
    assert_ok(s1, &b1, StatusCode::OK);
    let (s2, b2) = post_json(
        &env,
        "/api/v1/downloaders/submit",
        &[("idempotency-key", "pig-m2")],
        sub_b1.clone(),
    )
    .await;
    assert_ok(s2, &b2, StatusCode::OK);
    let (b_plan, b_ctx) = build_plan_and_ctx(
        (site1, &enc1, "Piggy.Show.S01E01.1080p.WEB-DL"),
        SubmissionPurpose::Boost,
        Some("cm.boost"),
        "pig-boost-1",
    );
    assert!(matches!(
        svc.submit_sync(b_plan, b_ctx).unwrap(),
        DownloadOutcome::Submitted {
            already_owned: true,
            ..
        }
    ));
    assert_eq!(
        env.state
            .store()
            .list_active_boost_task_facts()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(dl.submitted().len(), 1);

    let (sd, bd) = req_authed(&env, "DELETE", &format!("/api/v1/downloaders/tasks/{h1}")).await;
    assert_ok(sd, &bd, StatusCode::OK);
    assert!(
        env.state
            .store()
            .list_active_boost_task_facts()
            .unwrap()
            .is_empty(),
        "DELETE /downloaders/tasks/{{hash}} must transition boost_task_facts to Cleaned"
    );

    let (s2_re, b2_re) = post_json(
        &env,
        "/api/v1/downloaders/submit",
        &[("idempotency-key", "pig-m2")],
        sub_b1,
    )
    .await;
    assert_ok(s2_re, &b2_re, StatusCode::OK);
    assert_eq!(
        dl.submitted().len(),
        2,
        "piggybacked Idempotency-Key must re-submit after task deletion"
    );
}

#[tokio::test]
async fn delete_subscription_tombstones_piggybacked_idem_keys_and_cleans_boost_facts() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let t2 = minimal_torrent_bytes("Piggy.Show.S01E02.1080p.WEB-DL", 4096);
    let h2 = torrent_info_hash(&t2).unwrap();
    let srv2 = StubTorrentServer::start(t2);
    let enc2 = srv2.url("piggy2.torrent");
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let env = make_env(&dir, "", dl.clone(), &[srv2.addr]);
    let site2 = seed_site(&env, "PiggySite2", &srv2.url(""), "demo");
    let svc = ApiDownloadService::new(env.state.clone());

    let sub_id = create_tv_sub(&env, "Piggy Show").await;
    let sub_b2 = json!({
        "download_url": enc2,
        "title": "Piggy.Show.S01E02.1080p.WEB-DL",
        "site_id": site2.to_string(),
        "subscribe_id": sub_id.to_string(),
    });
    let piggy_b2 = json!({
        "download_url": enc2,
        "title": "Piggy.Show.S01E02.1080p.WEB-DL",
        "site_id": site2.to_string(),
    });
    let (ss1, bs1) = post_json(
        &env,
        "/api/v1/downloaders/submit",
        &[("idempotency-key", "pig-sub-1")],
        sub_b2,
    )
    .await;
    assert_ok(ss1, &bs1, StatusCode::OK);
    let (ss2, bs2) = post_json(
        &env,
        "/api/v1/downloaders/submit",
        &[("idempotency-key", "pig-sub-2")],
        piggy_b2.clone(),
    )
    .await;
    assert_ok(ss2, &bs2, StatusCode::OK);
    let (bp2, bc2) = build_plan_and_ctx(
        (site2, &enc2, "Piggy.Show.S01E02.1080p.WEB-DL"),
        SubmissionPurpose::Boost,
        Some("cm.boost"),
        "pig-boost-2",
    );
    svc.submit_sync(bp2, bc2).unwrap();
    assert_eq!(
        env.state
            .store()
            .list_active_boost_task_facts()
            .unwrap()
            .len(),
        1
    );

    let (s_del_sub, b_del_sub) = req_authed(
        &env,
        "DELETE",
        &format!("/api/v1/subscriptions/{sub_id}?delete_torrents=true"),
    )
    .await;
    assert_ok(s_del_sub, &b_del_sub, StatusCode::OK);
    assert!(
        env.state
            .store()
            .list_active_boost_task_facts()
            .unwrap()
            .is_empty()
    );
    let nil_dl = DownloaderId::from_uuid(Uuid::nil());
    assert!(
        env.state
            .store()
            .find_download_ownership_by_hash(nil_dl, &h2)
            .unwrap()
            .iter()
            .all(|o| o.ownership_state == DownloadOwnershipState::Deleted)
    );
    let (ss2_re, bs2_re) = post_json(
        &env,
        "/api/v1/downloaders/submit",
        &[("idempotency-key", "pig-sub-2")],
        piggy_b2,
    )
    .await;
    assert_ok(ss2_re, &bs2_re, StatusCode::OK);
    assert_eq!(dl.submitted().len(), 2);
}

struct BlindIdentityDownloader(MemoryDownloader);

impl Downloader for BlindIdentityDownloader {
    fn owned_identity(&self, _torrent: &Torrent) -> Result<Option<String>, DownloaderError> {
        Ok(None)
    }
    fn remove_owned(&self, torrent: &Torrent, delete_files: bool) -> Result<(), DownloaderError> {
        self.0.remove_owned(torrent, delete_files)
    }
    fn remember_identity(&self, torrent: &Torrent, hash: &str) -> Result<(), DownloaderError> {
        self.0.remember_identity(torrent, hash)
    }
    fn submit_verified(
        &self,
        payload: &SubmitPayload,
        options: &SubmitOptions,
    ) -> Result<ClientSubmitResult, DownloaderError> {
        self.0.submit_verified(payload, options)
    }
    fn completed_files(&self, t: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        self.0.completed_files(t)
    }
    fn task_snapshots(&self) -> Result<Vec<TaskSnapshot>, DownloaderError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn remove_task_falls_back_to_stored_pending_hash_when_client_identity_is_none() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let t_bytes = minimal_torrent_bytes("StoredHash.S01E01.1080p.WEB-DL", 4096);
    let expected_hash = torrent_info_hash(&t_bytes).unwrap();
    let srv = StubTorrentServer::start(t_bytes);
    let enc = srv.url("stored-e01.torrent");
    let html = nexus_row("StoredHash.S01E01.1080p.WEB-DL", &enc);
    let dl = Arc::new(BlindIdentityDownloader(MemoryDownloader::new(
        dir.path().join("stage"),
    )));
    let env = make_env(&dir, &html, dl.clone(), &[srv.addr]);
    let site_id = seed_site(&env, "StoredHashSite", &srv.url(""), "demo");
    let sub_id = create_tv_sub(&env, "StoredHash").await;

    // Admit first (records pending + pending_info_hash), then piggyback manual submit + Boost fact!
    let admit_body = json!({ "subscribe_id": sub_id.to_string(), "enclosure": enc });
    let (sa1, ba1) = post_json(
        &env,
        "/api/v1/search/admit",
        &[("idempotency-key", "sh-admit-1")],
        admit_body.clone(),
    )
    .await;
    assert_ok(sa1, &ba1, StatusCode::OK);
    let sub_body = json!({
        "download_url": enc,
        "title": "StoredHash.S01E01.1080p.WEB-DL",
        "site_id": site_id.to_string(),
    });
    let (sm1, bm1) = post_json(
        &env,
        "/api/v1/downloaders/submit",
        &[("idempotency-key", "sh-piggy-man")],
        sub_body.clone(),
    )
    .await;
    assert_ok(sm1, &bm1, StatusCode::OK);
    let (b_plan, b_ctx) = build_plan_and_ctx(
        (site_id, &enc, "StoredHash.S01E01.1080p.WEB-DL"),
        SubmissionPurpose::Boost,
        Some("cm.boost"),
        "sh-boost-1",
    );
    ApiDownloadService::new(env.state.clone())
        .submit_sync(b_plan, b_ctx)
        .unwrap();
    assert_eq!(
        env.state
            .store()
            .load_pending_info_hashes(sub_id)
            .unwrap()
            .get(&enc)
            .map(String::as_str),
        Some(expected_hash.as_str())
    );
    assert_eq!(
        env.state
            .store()
            .list_active_boost_task_facts()
            .unwrap()
            .len(),
        1
    );

    let rm_body = json!({ "task_id": format!("{sub_id}:{enc}"), "delete_files": false });
    let (sr, br) = post_json(&env, "/api/v1/downloaders/tasks/remove", &[], rm_body).await;
    assert_ok(sr, &br, StatusCode::OK);

    assert!(
        env.state
            .store()
            .list_active_boost_task_facts()
            .unwrap()
            .is_empty(),
        "remove_task stored_hash fallback must clean boost_task_facts even when owned_identity is None"
    );
    let (sm2, bm2) = post_json(
        &env,
        "/api/v1/downloaders/submit",
        &[("idempotency-key", "sh-piggy-man")],
        sub_body,
    )
    .await;
    assert_ok(sm2, &bm2, StatusCode::OK);
    assert_eq!(dl.0.submitted().len(), 2);
}

// --- Item 5: plugin_actions propagates Err(err) from check_management_replay ---

#[tokio::test]
async fn site_plugin_actions_mismatched_idempotency_key_returns_409_conflict() {
    let env = HttpEnv::new().await;
    let site_id = seed_site(&env, "IdemConflictSite", "http://127.0.0.1:9/", "pterclub");
    let obs = env.state.store().observe_site(site_id).unwrap().unwrap();
    let ifm = format!("\"{}\"", obs.revision());
    let hdr = [
        ("if-match", ifm.as_str()),
        ("idempotency-key", "pa-r5-idem-1"),
    ];
    let uri = format!("/api/v1/sites/{site_id}/plugin-actions");

    let (st1, b1) = post_json(&env, &uri, &hdr, json!({ "action": "checkin" })).await;
    assert_ok(st1, &b1, StatusCode::ACCEPTED);

    // Same Idempotency-Key with a mismatched payload (unsupported provider_id) MUST immediately
    // fail at check_management_replay with 409 plugin.idempotency_conflict rather than falling
    // through to 400 site.maintenance_unsupported or 409 plugin.conflict!
    let (st2, b2) = post_json(
        &env,
        &uri,
        &hdr,
        json!({ "action": "verify", "provider_id": "org.missing:none" }),
    )
    .await;
    assert_err(
        st2,
        &b2,
        StatusCode::CONFLICT,
        "plugin.idempotency_conflict",
    );
}
