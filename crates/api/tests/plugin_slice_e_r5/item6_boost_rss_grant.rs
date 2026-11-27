use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::{DownloaderId, MaintenanceVia, Site, SiteId, Torrent};
use indexer::{IndexerError, RegistrySnapshot};
use serde_json::{Value, json};

use crate::run_boost_cmd;
use crate::support::plugin_http_support::{HttpEnv, TOKEN, assert_ok, req_json};
use crate::support::{BoostTestDownloader, NoopFetcher, StubTorrentServer};

struct CountingRssProvider {
    rss_url: String,
    search_calls: Arc<AtomicUsize>,
    rss_calls: Arc<AtomicUsize>,
    desc: indexer::ProviderDescriptor,
}

impl indexer::IndexerProvider for CountingRssProvider {
    fn descriptor(&self) -> &indexer::ProviderDescriptor {
        &self.desc
    }
    fn search(
        &self,
        _: &indexer::ProviderCallContext<'_>,
        _: &Site,
        _: &indexer::SearchQuery<'_>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        self.search_calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![])
    }
    fn rss(
        &self,
        _: &indexer::ProviderCallContext<'_>,
        site: &Site,
        _: Option<&str>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        self.rss_calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![Torrent {
            site_id: site.id,
            id: Some("rss-1".into()),
            title: "Rss.Free.Candidate.2026".into(),
            enclosure: self.rss_url.clone(),
            size_bytes: Some(2048),
            seeders: Some(12),
            leechers: Some(1),
            snatched: None,
            upload_time: None,
            poster_url: None,
            free: true,
            hr: Some(false),
            imdb_id: None,
            detail_url: None,
            category: None,
        }])
    }
    fn resolve_download(
        &self,
        _: &indexer::ProviderCallContext<'_>,
        _: &Site,
        _: &Torrent,
    ) -> Result<Option<indexer::ResolvedDownload>, IndexerError> {
        Ok(None)
    }
}

async fn revoke_boost_cap(env: &HttpEnv, cap: &str, idem: &str) {
    let rev0 = env.get_plugin_rev("builtin.boost").await;
    env.disable_plugin("builtin.boost", rev0, &format!("{idem}-dis"))
        .await;
    let rev_dis = env.get_plugin_rev("builtin.boost").await;
    let grants = json!([{
        "capability": cap,
        "scope": "Global",
        "sensitive": false
    }]);
    let (st_rv, _, b_rv) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/builtin.boost/permissions/revoke",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev_dis}\"")),
                ("idempotency-key", idem),
            ],
            Some(json!({ "grants": grants, "reason": "r5-test" })),
        ))
        .await;
    assert_ok(st_rv, &b_rv, StatusCode::ACCEPTED);
    let rev_rv = env.get_plugin_rev("builtin.boost").await;
    env.enable_plugin("builtin.boost", rev_rv, &format!("{idem}-en"))
        .await;
}

async fn grant_boost_cap(env: &HttpEnv, cap: &str, idem: &str) {
    let rev0 = env.get_plugin_rev("builtin.boost").await;
    env.disable_plugin("builtin.boost", rev0, &format!("{idem}-dis"))
        .await;
    let rev_dis = env.get_plugin_rev("builtin.boost").await;
    let grants = json!([{
        "capability": cap,
        "scope": "Global",
        "sensitive": false
    }]);
    let (st_prep, _, b_prep) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/builtin.boost/permissions/prepare",
            Some(TOKEN),
            &[],
            Some(json!({ "requested_grants": grants })),
        ))
        .await;
    let prep: Value = assert_ok(st_prep, &b_prep, StatusCode::OK);
    let (st_gr, _, b_gr) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/builtin.boost/permissions/grant",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev_dis}\"")),
                ("idempotency-key", idem),
            ],
            Some(json!({ "consent_token": prep["consent_token"], "grants": grants })),
        ))
        .await;
    assert_ok(st_gr, &b_gr, StatusCode::ACCEPTED);
    let rev_gr = env.get_plugin_rev("builtin.boost").await;
    env.enable_plugin("builtin.boost", rev_gr, &format!("{idem}-en"))
        .await;
}

fn make_rss_candidate(
    rss_url: String,
    search_calls: Arc<AtomicUsize>,
    rss_calls: Arc<AtomicUsize>,
) -> indexer::RegistryCandidate {
    let desc = indexer::ProviderDescriptor {
        id: "r5_rss_idx".into(),
        name: "R5RssStub".into(),
        kind: "builtin".into(),
        source: indexer::ProviderSource::Builtin,
        auth_types: vec![indexer::AuthType::Cookie],
        capabilities: vec![
            indexer::ProviderCapability::Search,
            indexer::ProviderCapability::Rss,
        ],
        availability: indexer::ProviderAvailability::Enabled,
        hidden_from_catalog: false,
    };
    indexer::RegistryCandidate::from_builtin()
        .unwrap()
        .add_indexer_provider(Arc::new(CountingRssProvider {
            rss_url,
            search_calls,
            rss_calls,
            desc,
        }))
        .unwrap()
}

#[tokio::test]
async fn slice_e_r5_item6_boost_cycle_enforces_indexer_rss_and_search_grants() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let srv = StubTorrentServer::start_dynamic(2048);
    let dl = Arc::new(BoostTestDownloader::new(dir.path().join("stage")));
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());

    let search_calls = Arc::new(AtomicUsize::new(0));
    let rss_calls = Arc::new(AtomicUsize::new(0));
    let make_reg = || {
        make_rss_candidate(
            srv.url("rss-cand-1.torrent"),
            search_calls.clone(),
            rss_calls.clone(),
        )
    };

    let state = ApiState::new_arc(
        store,
        TOKEN.into(),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl,
    )
    .unwrap()
    .with_plugin_http_policy_override(
        net_guard::TargetPolicy::production().with_test_loopback_socket(srv.addr()),
    );
    state.indexer().publish(make_reg()).unwrap();
    let site_id = SiteId::new();
    state
        .store()
        .insert_site(&Site {
            id: site_id,
            name: "PT-RssOnly".into(),
            url: "https://pt.example/".into(),
            profile_id: "r5_rss_idx".into(),
            cookie: Some("uid=1".into()),
            api_key: None,
            rss_url: Some("https://pt.example/rss".into()),
            proxy: None,
            rate_limit_per_minute: Some(60),
            cdp_url: None,
            downloader_id: None,
            enabled: true,
            maintenance_via: MaintenanceVia::Http,
            browser_search: false,
            timezone: "UTC".into(),
        })
        .unwrap();
    let dl_id = DownloaderId::new();
    state
        .store()
        .insert_downloader(&store::DownloaderRow {
            id: dl_id,
            name: "R5CycleDL".into(),
            kind: "qbittorrent".into(),
            url: "http://127.0.0.1:9".into(),
            username: None,
            password: None,
            api_key: None,
            category: None,
            path_maps: vec![],
            is_default: true,
            enabled: true,
        })
        .unwrap();
    let env = HttpEnv {
        _dir: dir,
        app: router(state.clone()),
        state,
        member_token: String::new(),
    };

    // Case A: search granted, rss revoked -> search runs (empty), RSS fallback MUST NOT run!
    revoke_boost_cap(&env, "command:indexer.rss", "r5-rev-rss").await;
    env.state.indexer().publish(make_reg()).unwrap();
    let op_no_rss = run_boost_cmd(
        &env,
        "r5-cycle-no-rss",
        "boost.run",
        json!({ "sites": [site_id], "downloader_id": dl_id, "dry_run": false }),
    )
    .await;
    assert_eq!(op_no_rss["state"], "succeeded", "op={op_no_rss}");
    assert_eq!(op_no_rss["result"]["added"], 0);
    assert_eq!(search_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        rss_calls.load(Ordering::SeqCst),
        0,
        "rss_with_snapshot must not run without command:indexer.rss grant"
    );

    // Case B: rss granted, search revoked -> authorized, skips search, uses RSS!
    grant_boost_cap(&env, "command:indexer.rss", "r5-gr-rss").await;
    revoke_boost_cap(&env, "command:indexer.search", "r5-rev-search").await;
    env.state.indexer().publish(make_reg()).unwrap();
    let op_rss_only = run_boost_cmd(
        &env,
        "r5-cycle-rss-only",
        "boost.run",
        json!({ "sites": [site_id], "downloader_id": dl_id, "dry_run": false }),
    )
    .await;
    assert_eq!(op_rss_only["state"], "succeeded", "op={op_rss_only}");
    assert_eq!(op_rss_only["result"]["added"], 1);
    assert_eq!(search_calls.load(Ordering::SeqCst), 1);
    assert_eq!(rss_calls.load(Ordering::SeqCst), 1);
}
