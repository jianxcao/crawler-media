use std::path::Path;
use std::sync::{Arc, Mutex};

use domain::{
    AtomRule, Coverage, FetchMode, Filter, FilterAtom, FilterId, Media, MediaId, MediaKind, SiteId,
    Subscribe, SubscribeFacts, SubscribeId, Torrent, UserId,
};
use downloader::{
    DownloadOpContext, DownloadOutcome, DownloadSubmitError, FrozenDownloadPlan, MemoryDownloader,
};
use hooks::{Gate, RescoreBudget};
use plugin_api::Candidate;
use subscribe::{RunInput, SubscribePolicyHooks, SyncDownloadSubmitter, admit_and_add};

struct SharedBudgetPolicy {
    order: Arc<Mutex<Vec<&'static str>>>,
}

impl SubscribePolicyHooks for SharedBudgetPolicy {
    fn filter_after(
        &self,
        _subscribe_id: SubscribeId,
        candidates: &mut Vec<Candidate>,
        _admitted: &mut Vec<String>,
        budget: &mut RescoreBudget,
    ) {
        self.order.lock().unwrap().push("filter.after");
        if let Some(c) = candidates.first_mut()
            && budget.try_apply("p.policy", &c.handle, 8).is_ok()
        {
            c.score = Some(c.score.unwrap_or(0) + 8);
        }
    }

    fn score(
        &self,
        _subscribe_id: SubscribeId,
        _media_id: MediaId,
        candidates: &mut Vec<Candidate>,
        _admitted: &mut Vec<String>,
        budget: &mut RescoreBudget,
    ) {
        self.order.lock().unwrap().push("subscribe.score");
        if let Some(c) = candidates.first_mut()
            && budget.try_apply("p.policy", &c.handle, 5).is_ok()
        {
            c.score = Some(c.score.unwrap_or(0) + 5);
        }
    }

    fn select(
        &self,
        _subscribe_id: SubscribeId,
        _media_id: MediaId,
        _candidates: &mut Vec<Candidate>,
        _admitted: &mut Vec<String>,
    ) -> Gate {
        Gate::Allow
    }
}

struct RecordingSubmitter {
    keys: Mutex<Vec<String>>,
    defer_first: Mutex<bool>,
}

impl SyncDownloadSubmitter for RecordingSubmitter {
    fn submit_sync(
        &self,
        _plan: FrozenDownloadPlan,
        op: DownloadOpContext,
    ) -> Result<DownloadOutcome, DownloadSubmitError> {
        self.keys.lock().unwrap().push(op.idempotency_key);
        let mut first = self.defer_first.lock().unwrap();
        if *first {
            *first = false;
            return Ok(DownloadOutcome::Deferred {
                operation_id: op.operation_id,
                until_unix: 100,
                reason: "rate-limit".into(),
            });
        }
        Ok(DownloadOutcome::Submitted {
            operation_id: op.operation_id,
            downloader_id: None,
            info_hash: Some("deadbeef".into()),
            bytes_digest: None,
            verified_total_bytes: Some(1024),
            already_owned: false,
        })
    }
}

#[test]
fn subscribe_filter_after_and_score_share_budget_and_retry_uses_distinct_idempotency_key() {
    let filter = Filter {
        id: FilterId::new(),
        name: "f".into(),
        atoms: vec![FilterAtom {
            priority: 80,
            rule: AtomRule::TitleMatch("Matrix".into()),
            exclude: false,
        }],
        keep_old_versions: false,
    };
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
    let torrent = Torrent {
        site_id: SiteId::new(),
        id: Some("1".into()),
        title: "The.Matrix.1999.1080p".into(),
        enclosure: "https://pt.example/m.torrent".into(),
        size_bytes: Some(1024),
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
    };
    let dl = MemoryDownloader::new(Path::new("/tmp/cm-sub-test"));
    let order = Arc::new(Mutex::new(Vec::new()));
    let hooks = SharedBudgetPolicy {
        order: Arc::clone(&order),
    };
    let submitter = RecordingSubmitter {
        keys: Mutex::new(Vec::new()),
        defer_first: Mutex::new(true),
    };

    let make_input = |t: Torrent| RunInput {
        subscribe: &sub,
        media: &media,
        filter: &filter,
        wash_filter: None,
        torrents: vec![t],
        search_keywords: vec![],
        facts: SubscribeFacts::default(),
        downloader: &dl,
        library_root: Path::new("/tmp/cm-sub-lib"),
        library_id: None,
        root_id: None,
        config_revision: None,
        transfer_mode: None,
        scrape: false,
        hooks: None,
        policy_hooks: Some(&hooks),
        submitter: Some(&submitter),
        naming: None,
        preserve_removed: false,
        overlap: Default::default(),
        absolute: None,
        renamed_destinations: Default::default(),
    };

    let r1 = admit_and_add(make_input(torrent.clone())).unwrap();
    assert_eq!(r1.deferred.len(), 1);
    assert_eq!(r1.deferred[0].0.score, 88);
    assert!(r1.select_deferred().is_empty());

    let r2 = admit_and_add(make_input(torrent)).unwrap();
    assert_eq!(r2.chosen.len(), 1);
    assert_eq!(r2.chosen[0].score, 88);
    assert_eq!(
        *order.lock().unwrap(),
        vec![
            "filter.after",
            "subscribe.score",
            "filter.after",
            "subscribe.score"
        ]
    );
    let keys = submitter.keys.lock().unwrap();
    assert_eq!(keys.len(), 2);
    assert_ne!(
        keys[0], keys[1],
        "retry must use fresh per-attempt idempotency key"
    );
}

struct CapturePublished(Arc<Mutex<Vec<Option<u64>>>>);

impl SubscribePolicyHooks for CapturePublished {
    fn filter_after(
        &self,
        _subscribe_id: SubscribeId,
        candidates: &mut Vec<Candidate>,
        _admitted: &mut Vec<String>,
        _budget: &mut RescoreBudget,
    ) {
        *self.0.lock().unwrap() = candidates.iter().map(|c| c.published_at).collect();
    }
    fn score(
        &self,
        _subscribe_id: SubscribeId,
        _media_id: MediaId,
        _candidates: &mut Vec<Candidate>,
        _admitted: &mut Vec<String>,
        _budget: &mut RescoreBudget,
    ) {
    }
    fn select(
        &self,
        _subscribe_id: SubscribeId,
        _media_id: MediaId,
        _candidates: &mut Vec<Candidate>,
        _admitted: &mut Vec<String>,
    ) -> Gate {
        Gate::Allow
    }
}

#[test]
fn subscribe_policy_hooks_receive_parsed_published_at_ms_across_timestamp_formats() {
    let filter = Filter {
        id: FilterId::new(),
        name: "f".into(),
        atoms: vec![],
        keep_old_versions: false,
    };
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
    let mk = |idx: usize, ts: &str| Torrent {
        site_id: SiteId::new(),
        id: Some(idx.to_string()),
        title: format!("The.Matrix.1999.1080p.WEB-DL-G{idx}"),
        enclosure: format!("magnet:?xt=urn:btih:{idx:040x}"),
        size_bytes: Some(1024),
        seeders: Some(10),
        leechers: Some(1),
        snatched: None,
        upload_time: Some(ts.into()),
        poster_url: None,
        free: false,
        hr: Some(false),
        imdb_id: None,
        detail_url: None,
        category: None,
    };
    let captured = Arc::new(Mutex::new(Vec::new()));
    let hooks = CapturePublished(Arc::clone(&captured));
    let dl = MemoryDownloader::new(Path::new("/tmp/cm-sub-ts"));
    admit_and_add(RunInput {
        subscribe: &sub,
        media: &media,
        filter: &filter,
        wash_filter: None,
        torrents: vec![
            mk(1, "2024-01-01T00:00:00Z"),
            mk(2, "Mon, 01 Jan 2024 00:00:00 +0000"),
            mk(3, "2024-01-01 00:00:00"),
            mk(4, "1704067200"),
        ],
        search_keywords: vec![],
        facts: SubscribeFacts::default(),
        downloader: &dl,
        library_root: Path::new("/tmp/cm-sub-lib"),
        library_id: None,
        root_id: None,
        config_revision: None,
        transfer_mode: None,
        scrape: false,
        hooks: None,
        policy_hooks: Some(&hooks),
        submitter: None,
        naming: None,
        preserve_removed: false,
        overlap: Default::default(),
        absolute: None,
        renamed_destinations: Default::default(),
    })
    .unwrap();
    assert_eq!(*captured.lock().unwrap(), vec![Some(1_704_067_200_000); 4]);
}

struct SelectFilterThenDeferPolicy;

impl SubscribePolicyHooks for SelectFilterThenDeferPolicy {
    fn filter_after(
        &self,
        _subscribe_id: SubscribeId,
        _candidates: &mut Vec<Candidate>,
        _admitted: &mut Vec<String>,
        _budget: &mut RescoreBudget,
    ) {
    }
    fn score(
        &self,
        _subscribe_id: SubscribeId,
        _media_id: MediaId,
        _candidates: &mut Vec<Candidate>,
        _admitted: &mut Vec<String>,
        _budget: &mut RescoreBudget,
    ) {
    }
    fn select(
        &self,
        _subscribe_id: SubscribeId,
        _media_id: MediaId,
        candidates: &mut Vec<Candidate>,
        admitted: &mut Vec<String>,
    ) -> Gate {
        admitted.retain(|h| h == "0");
        candidates.retain(|c| c.handle == "0");
        Gate::Defer {
            until_ms: 15_000,
            reason: "select-cooldown".into(),
        }
    }
}

#[test]
fn subscribe_select_defer_drops_filtered_handles_and_populates_select_deferred() {
    let filter = Filter {
        id: FilterId::new(),
        name: "tv".into(),
        atoms: vec![],
        keep_old_versions: false,
    };
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Show".into(),
        year: Some(2025),
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let sub = Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id: media.id,
        coverage: Coverage::Tv {
            season: 1,
            episode_from: 1,
            episode_to: Some(2),
        },
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
    let mk_ep = |ep: u32| Torrent {
        site_id: SiteId::new(),
        id: Some(ep.to_string()),
        title: format!("Show.S01E0{ep}.1080p.WEB-DL"),
        enclosure: format!("https://pt.example/ep{ep}.torrent"),
        size_bytes: Some(1024),
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
    };
    let dl = MemoryDownloader::new(Path::new("/tmp/cm-sub-select-defer"));
    let policy = SelectFilterThenDeferPolicy;
    let added = admit_and_add(RunInput {
        subscribe: &sub,
        media: &media,
        filter: &filter,
        wash_filter: None,
        torrents: vec![mk_ep(1), mk_ep(2)],
        search_keywords: vec![],
        facts: SubscribeFacts::default(),
        downloader: &dl,
        library_root: Path::new("/tmp/cm-sub-lib"),
        library_id: None,
        root_id: None,
        config_revision: None,
        transfer_mode: None,
        scrape: false,
        hooks: None,
        policy_hooks: Some(&policy),
        submitter: None,
        naming: None,
        preserve_removed: false,
        overlap: Default::default(),
        absolute: None,
        renamed_destinations: Default::default(),
    })
    .unwrap();

    assert!(added.chosen.is_empty());
    assert_eq!(
        added.deferred.len(),
        1,
        "dropped handle 1 must not be deferred"
    );
    assert_eq!(
        added.deferred[0].0.torrent.enclosure,
        "https://pt.example/ep1.torrent"
    );
    assert_eq!(added.select_deferred().len(), 1);
    assert_eq!(
        added.select_deferred()[0].0.torrent.enclosure,
        "https://pt.example/ep1.torrent"
    );
}

struct DeferOnSecondRoundPolicy(Mutex<usize>);

impl SubscribePolicyHooks for DeferOnSecondRoundPolicy {
    fn filter_after(
        &self,
        _subscribe_id: SubscribeId,
        _candidates: &mut Vec<Candidate>,
        _admitted: &mut Vec<String>,
        _budget: &mut RescoreBudget,
    ) {
    }
    fn score(
        &self,
        _subscribe_id: SubscribeId,
        _media_id: MediaId,
        _candidates: &mut Vec<Candidate>,
        _admitted: &mut Vec<String>,
        _budget: &mut RescoreBudget,
    ) {
    }
    fn select(
        &self,
        _subscribe_id: SubscribeId,
        _media_id: MediaId,
        _candidates: &mut Vec<Candidate>,
        _admitted: &mut Vec<String>,
    ) -> Gate {
        let mut round = self.0.lock().unwrap();
        *round += 1;
        if *round == 1 {
            Gate::Allow
        } else {
            Gate::Defer {
                until_ms: 15_000,
                reason: "fallback-cooldown".into(),
            }
        }
    }
}

struct RejectFirstSubmitter;

impl SyncDownloadSubmitter for RejectFirstSubmitter {
    fn submit_sync(
        &self,
        plan: FrozenDownloadPlan,
        op: DownloadOpContext,
    ) -> Result<DownloadOutcome, DownloadSubmitError> {
        if plan.torrent.enclosure.ends_with("/dead.torrent") {
            return Err(DownloadSubmitError::Downloader(
                downloader::DownloaderError::SourceRejected("404".into()),
            ));
        }
        Ok(DownloadOutcome::Submitted {
            operation_id: op.operation_id,
            downloader_id: None,
            info_hash: Some("deadbeef".into()),
            bytes_digest: None,
            verified_total_bytes: Some(1024),
            already_owned: false,
        })
    }
}

#[test]
fn subscribe_select_defer_after_source_fallback_populates_select_deferred() {
    let filter = Filter {
        id: FilterId::new(),
        name: "hd".into(),
        atoms: vec![
            FilterAtom {
                priority: 100,
                rule: AtomRule::Resolution("2160p".into()),
                exclude: false,
            },
            FilterAtom {
                priority: 50,
                rule: AtomRule::Resolution("1080p".into()),
                exclude: false,
            },
        ],
        keep_old_versions: false,
    };
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
    let mk = |res: &str, file: &str| Torrent {
        site_id: SiteId::new(),
        id: Some(file.into()),
        title: format!("The.Matrix.1999.{res}.BluRay"),
        enclosure: format!("https://pt.example/{file}"),
        size_bytes: Some(1024),
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
    };
    let dl = MemoryDownloader::new(Path::new("/tmp/cm-sub-fallback-defer"));
    let policy = DeferOnSecondRoundPolicy(Mutex::new(0));
    let submitter = RejectFirstSubmitter;
    let added = admit_and_add(RunInput {
        subscribe: &sub,
        media: &media,
        filter: &filter,
        wash_filter: None,
        torrents: vec![mk("2160p", "dead.torrent"), mk("1080p", "backup.torrent")],
        search_keywords: vec![],
        facts: SubscribeFacts::default(),
        downloader: &dl,
        library_root: Path::new("/tmp/cm-sub-lib"),
        library_id: None,
        root_id: None,
        config_revision: None,
        transfer_mode: None,
        scrape: false,
        hooks: None,
        policy_hooks: Some(&policy),
        submitter: Some(&submitter),
        naming: None,
        preserve_removed: false,
        overlap: Default::default(),
        absolute: None,
        renamed_destinations: Default::default(),
    })
    .unwrap();

    assert_eq!(
        added.outcome.rejected_enclosures,
        vec!["https://pt.example/dead.torrent".to_string()]
    );
    assert_eq!(added.select_deferred().len(), 1);
    assert_eq!(
        added.select_deferred()[0].0.torrent.enclosure,
        "https://pt.example/backup.torrent"
    );
}
