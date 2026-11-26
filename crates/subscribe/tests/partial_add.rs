use std::path::PathBuf;

use domain::SubscribeFacts;
use domain::{
    AtomRule, Coverage, FetchMode, Filter, FilterAtom, FilterId, Media, MediaId, MediaKind,
    Subscribe, SubscribeId, Torrent, UserId,
};
use downloader::{Downloader, DownloaderError};
use subscribe::{RunInput, admit_and_add};

struct FailsOne;

impl Downloader for FailsOne {
    fn add(&self, torrent: &Torrent) -> Result<(), DownloaderError> {
        if torrent.enclosure.ends_with("/bad") {
            Err(DownloaderError::Message("injected failure".into()))
        } else {
            Ok(())
        }
    }
    fn completed_files(&self, _: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        Ok(vec![])
    }
}

fn torrent(title: &str, suffix: &str) -> Torrent {
    Torrent {
        site_id: domain::SiteId::new(),
        title: title.into(),
        enclosure: format!("https://pt.example/{suffix}"),
        size_bytes: None,
        seeders: None,
        free: false,
        hr: Some(false),
        imdb_id: None,
        id: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    }
}

#[test]
fn successful_download_additions_are_returned_when_another_add_fails() {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "The Expanse".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let filter = Filter {
        id: FilterId::new(),
        name: "1080p".into(),
        atoms: vec![FilterAtom {
            priority: 10,
            rule: AtomRule::Resolution("1080p".into()),
            exclude: false,
        }],
        keep_old_versions: false,
    };
    let subscribe = Subscribe {
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
    let second_release = release::parse("The.Expanse.2020.S01E02.1080p.WEB-DL");
    assert_eq!(second_release.episode, Some(2));
    assert!(subscribe::candidate_matches_subscribe(
        &subscribe,
        &media,
        &second_release
    ));
    let downloader = FailsOne;
    let added = admit_and_add(RunInput {
        subscribe: &subscribe,
        media: &media,
        filter: &filter,
        wash_filter: None,
        torrents: vec![
            torrent("The.Expanse.2020.S01E01.1080p.WEB-DL", "good"),
            torrent("The.Expanse.2020.S01E02.1080p.WEB-DL", "bad"),
        ],
        search_keywords: vec![],
        facts: SubscribeFacts::default(),
        downloader: &downloader,
        library_root: std::path::Path::new("/tmp/library"),
        library_id: None,
        root_id: None,
        config_revision: None,
        transfer_mode: None,
        scrape: false,
        hooks: None,
        naming: None,
        preserve_removed: false,
        overlap: Default::default(),
        absolute: None,
    })
    .unwrap();

    assert_eq!(added.chosen.len(), 1);
    assert_eq!(added.chosen[0].torrent.enclosure, "https://pt.example/good");
    assert_eq!(added.add_errors.len(), 1);
}

struct FallbackDownloader {
    source_fail_suffix: &'static str,
    client_fail_suffix: &'static str,
    calls: std::sync::Mutex<Vec<String>>,
}

impl Downloader for FallbackDownloader {
    fn add(&self, torrent: &Torrent) -> Result<(), DownloaderError> {
        self.calls.lock().unwrap().push(torrent.enclosure.clone());
        if torrent.enclosure.ends_with(self.source_fail_suffix) {
            return Err(DownloaderError::SourceRejected(
                "HTTP 404 fetching .torrent".into(),
            ));
        }
        if torrent.enclosure.ends_with(self.client_fail_suffix) {
            return Err(DownloaderError::Message(
                "qBittorrent connection refused".into(),
            ));
        }
        Ok(())
    }
    fn completed_files(&self, _: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        Ok(vec![])
    }
}

#[test]
fn source_rejected_falls_back_to_next_best_candidate_in_same_round() {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: Some(1999),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
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
    let subscribe = Subscribe {
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
    let downloader = FallbackDownloader {
        source_fail_suffix: "/dead-2160p",
        client_fail_suffix: "/never",
        calls: std::sync::Mutex::new(Vec::new()),
    };
    let added = admit_and_add(RunInput {
        subscribe: &subscribe,
        media: &media,
        filter: &filter,
        wash_filter: None,
        torrents: vec![
            torrent("The.Matrix.1999.2160p.BluRay", "dead-2160p"),
            torrent("The.Matrix.1999.1080p.BluRay", "good-1080p"),
        ],
        search_keywords: vec![],
        facts: SubscribeFacts::default(),
        downloader: &downloader,
        library_root: std::path::Path::new("/tmp/library"),
        library_id: None,
        root_id: None,
        config_revision: None,
        transfer_mode: None,
        scrape: false,
        hooks: None,
        naming: None,
        preserve_removed: false,
        overlap: Default::default(),
        absolute: None,
    })
    .unwrap();

    assert_eq!(added.chosen.len(), 1);
    assert_eq!(
        added.chosen[0].torrent.enclosure,
        "https://pt.example/good-1080p"
    );
    assert!(added.add_errors.is_empty(), "{:?}", added.add_errors);
    assert_eq!(
        added.outcome.rejected_enclosures,
        vec!["https://pt.example/dead-2160p".to_string()]
    );
    assert_eq!(
        *downloader.calls.lock().unwrap(),
        vec![
            "https://pt.example/dead-2160p",
            "https://pt.example/good-1080p"
        ]
    );
}

#[test]
fn tv_source_rejected_falls_back_per_episode_without_retrying_client_errors() {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "The Expanse".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
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
    let subscribe = Subscribe {
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
    let downloader = FallbackDownloader {
        source_fail_suffix: "/ep1-dead",
        client_fail_suffix: "/ep2-client-err",
        calls: std::sync::Mutex::new(Vec::new()),
    };
    let added = admit_and_add(RunInput {
        subscribe: &subscribe,
        media: &media,
        filter: &filter,
        wash_filter: None,
        torrents: vec![
            torrent("The.Expanse.S01E01.2160p.WEB-DL", "ep1-dead"),
            torrent("The.Expanse.S01E01.1080p.WEB-DL", "ep1-backup"),
            torrent("The.Expanse.S01E02.2160p.WEB-DL", "ep2-client-err"),
            torrent("The.Expanse.S01E02.1080p.WEB-DL", "ep2-backup"),
        ],
        search_keywords: vec![],
        facts: SubscribeFacts::default(),
        downloader: &downloader,
        library_root: std::path::Path::new("/tmp/library"),
        library_id: None,
        root_id: None,
        config_revision: None,
        transfer_mode: None,
        scrape: false,
        hooks: None,
        naming: None,
        preserve_removed: false,
        overlap: Default::default(),
        absolute: None,
    })
    .unwrap();

    assert_eq!(added.chosen.len(), 1);
    assert_eq!(
        added.chosen[0].torrent.enclosure,
        "https://pt.example/ep1-backup"
    );
    assert_eq!(added.add_errors.len(), 1);
    assert_eq!(
        added.outcome.rejected_enclosures,
        vec!["https://pt.example/ep1-dead".to_string()]
    );
    // EP2 client error must NOT retry ep2-backup
    let calls = downloader.calls.lock().unwrap();
    assert!(
        !calls.iter().any(|u| u.ends_with("/ep2-backup")),
        "{calls:?}"
    );
}

#[test]
fn collect_completed_and_run_preserve_submission_rejected_enclosures() {
    struct BrokenAndRejectedDownloader;
    impl Downloader for BrokenAndRejectedDownloader {
        fn add(&self, torrent: &Torrent) -> Result<(), DownloaderError> {
            if torrent.enclosure.ends_with("/broken") {
                return Err(DownloaderError::ExistingTaskBroken("deadbeef".into()));
            }
            if torrent.enclosure.ends_with("/rejected") {
                return Err(DownloaderError::SourceRejected("HTTP 404".into()));
            }
            Ok(())
        }
        fn completed_files(&self, _: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
            Ok(vec![])
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: Some(1999),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let filter = Filter {
        id: FilterId::new(),
        name: "hd".into(),
        atoms: vec![
            FilterAtom {
                priority: 120,
                rule: AtomRule::Resolution("2160p".into()),
                exclude: false,
            },
            FilterAtom {
                priority: 80,
                rule: AtomRule::Resolution("1080p".into()),
                exclude: false,
            },
            FilterAtom {
                priority: 40,
                rule: AtomRule::Resolution("720p".into()),
                exclude: false,
            },
        ],
        keep_old_versions: false,
    };
    let subscribe = Subscribe {
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
    let downloader = BrokenAndRejectedDownloader;
    let outcome = subscribe::run(RunInput {
        subscribe: &subscribe,
        media: &media,
        filter: &filter,
        wash_filter: None,
        torrents: vec![
            torrent("The.Matrix.1999.2160p.BluRay", "broken"),
            torrent("The.Matrix.1999.1080p.BluRay", "rejected"),
            torrent("The.Matrix.1999.720p.BluRay", "usable"),
        ],
        search_keywords: vec![],
        facts: SubscribeFacts::default(),
        downloader: &downloader,
        library_root: tmp.path(),
        library_id: None,
        root_id: None,
        config_revision: None,
        transfer_mode: None,
        scrape: false,
        hooks: None,
        naming: None,
        preserve_removed: false,
        overlap: Default::default(),
        absolute: None,
    })
    .unwrap();

    assert_eq!(outcome.torrents_added.len(), 1);
    assert_eq!(
        outcome.torrents_added[0].1.enclosure,
        "https://pt.example/usable"
    );
    assert_eq!(
        outcome.rejected_enclosures,
        vec![
            "https://pt.example/broken".to_string(),
            "https://pt.example/rejected".to_string(),
        ]
    );
}
