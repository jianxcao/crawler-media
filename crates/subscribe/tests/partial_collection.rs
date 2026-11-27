use std::path::{Path, PathBuf};

use domain::SubscribeFacts;
use domain::{
    AtomRule, Confidence, Coverage, FetchMode, Filter, FilterAtom, FilterId, LedgerId, Media,
    MediaId, MediaKind, QualitySource, Subscribe, SubscribeId, Torrent, UserId,
};
use downloader::{Downloader, DownloaderError};
use filter::ScoredTorrent;
use library::{FileQuality, TransferMode};
use subscribe::{Added, RunInput, RunOutcome, collect_completed};

struct FixedFiles(Vec<PathBuf>);

impl Downloader for FixedFiles {
    fn submit_verified(
        &self,
        payload: &downloader::SubmitPayload,
        _options: &downloader::SubmitOptions,
    ) -> Result<downloader::ClientSubmitResult, DownloaderError> {
        Ok(downloader::ClientSubmitResult::Added {
            already_owned: false,
            info_hash: payload.info_hash().to_string(),
        })
    }
    fn completed_files(&self, _: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        Ok(self.0.clone())
    }
}

struct Probe;

impl library::MediaProbe for Probe {
    fn probe(&self, _: &Path) -> Result<FileQuality, library::LibraryError> {
        Ok(FileQuality {
            resolution: Some("1080p".into()),
            codec: None,
            hdr: None,
        })
    }
}

#[test]
fn one_failed_file_does_not_discard_successful_files_from_same_torrent() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_fixture();
    let filter = filter_fixture();
    let subscribe = subscribe_fixture(&media, &filter);
    let first = tmp.path().join("The.Expanse.S01E01.mkv");
    std::fs::write(&first, b"video").unwrap();
    let missing = tmp.path().join("The.Expanse.S01E02.mkv");
    let downloader = FixedFiles(vec![first, missing]);
    let torrent = torrent_fixture();
    let added = Added {
        input: RunInput {
            subscribe: &subscribe,
            media: &media,
            filter: &filter,
            wash_filter: None,
            torrents: vec![],
            search_keywords: vec![],
            facts: SubscribeFacts::default(),
            downloader: &downloader,
            library_root: &tmp.path().join("library"),
            library_id: Some(domain::LibraryId::new()),
            root_id: Some("test-root".into()),
            config_revision: None,
            transfer_mode: Some(TransferMode::Copy),
            scrape: false,
            hooks: None,
            policy_hooks: None,
            submitter: None,
            naming: None,
            preserve_removed: false,
            overlap: Default::default(),
            absolute: None,
            renamed_destinations: Default::default(),
        },
        chosen: vec![ScoredTorrent {
            release: domain::Release {
                title: torrent.title.clone(),
                year: None,
                season: Some(1),
                episode: Some(1),
                episode_to: Some(2),
                resolution: Some("1080p".into()),
                source: None,
                codec: None,
                hdr: None,
                subtitle_language: None,
                audio_language: None,
                group: None,
                confidence: Confidence::High,
                season_inferred: false,
            },
            torrent,
            score: 80,
        }],
        add_errors: vec![],
        rejected: vec![],
        outcome: RunOutcome {
            facts: SubscribeFacts::default(),
            files: vec![],
            file_sources: vec![],
            removed_paths: vec![],
            completed: false,
            transferred_enclosures: vec![],
            rejected_enclosures: Vec::new(),
            awaiting_probe_enclosures: Vec::new(),
            collection_errors: vec![],
            submission_errors: vec![],
            torrents_added: vec![],
            deferred_enclosures: vec![],
            unknown_enclosures: vec![],
            submitted_meta: Default::default(),
        },
        broken: Vec::new(),
        deferred: Vec::new(),
        select_deferred: Vec::new(),
        unknown: Vec::new(),
        submitted_meta: Default::default(),
    };

    let outcome = collect_completed(added, &Probe)
        .expect("partial collection should return persistable results");

    assert_eq!(outcome.files.len(), 1);
    assert_eq!(outcome.files[0].units, vec![domain::UnitKey::new(1, 1)]);
    assert_eq!(outcome.collection_errors.len(), 1);
    assert!(outcome.transferred_enclosures.is_empty());
    assert_eq!(outcome.files[0].file.quality_source, QualitySource::Probe);
    assert_ne!(outcome.files[0].file.file_id, LedgerId::new());
}

#[test]
fn preplanned_renamed_destination_applies_without_transfer_gate() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_fixture();
    let filter = filter_fixture();
    let subscribe = subscribe_fixture(&media, &filter);
    let first = tmp.path().join("The.Expanse.S01E01.mkv");
    std::fs::write(&first, b"video").unwrap();
    let downloader = FixedFiles(vec![first.clone()]);
    let transfer_gate_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&transfer_gate_called);
    let pipeline = hooks::HookPipeline::new();
    pipeline.register(hooks::Registration::new(
        0,
        "p",
        "transfer",
        std::sync::Arc::new(move |context: &hooks::GateContext| {
            if matches!(context, hooks::GateContext::Transfer { .. }) {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                return Ok(hooks::Gate::Reject("blocked".into()));
            }
            Ok(hooks::Gate::Abstain)
        }),
    ));
    let marker = tmp.path().join("reserved");
    let recorder = Recorder(&marker);
    let torrent = torrent_fixture();
    let lib_root = tmp.path().join("library");
    let renamed_dest = lib_root.join("renamed/Custom/The.Expanse.S01E01.mkv");
    let mut renamed_destinations = std::collections::HashMap::new();
    renamed_destinations.insert(first, renamed_dest.clone());
    let added = Added {
        input: RunInput {
            subscribe: &subscribe,
            media: &media,
            filter: &filter,
            wash_filter: None,
            torrents: vec![],
            search_keywords: vec![],
            facts: SubscribeFacts::default(),
            downloader: &downloader,
            library_root: &lib_root,
            library_id: Some(domain::LibraryId::new()),
            root_id: Some("test-root".into()),
            config_revision: None,
            transfer_mode: Some(TransferMode::Copy),
            scrape: false,
            hooks: Some(&pipeline),
            policy_hooks: None,
            submitter: None,
            naming: None,
            preserve_removed: false,
            overlap: Default::default(),
            absolute: None,
            renamed_destinations,
        },
        chosen: vec![ScoredTorrent {
            release: domain::Release {
                title: torrent.title.clone(),
                year: None,
                season: Some(1),
                episode: Some(1),
                episode_to: Some(2),
                resolution: Some("1080p".into()),
                source: None,
                codec: None,
                hdr: None,
                subtitle_language: None,
                audio_language: None,
                group: None,
                confidence: Confidence::High,
                season_inferred: false,
            },
            torrent,
            score: 80,
        }],
        add_errors: vec![],
        rejected: vec![],
        outcome: RunOutcome {
            facts: SubscribeFacts::default(),
            files: vec![],
            file_sources: vec![],
            removed_paths: vec![],
            completed: false,
            transferred_enclosures: vec![],
            rejected_enclosures: Vec::new(),
            awaiting_probe_enclosures: Vec::new(),
            collection_errors: vec![],
            submission_errors: vec![],
            torrents_added: vec![],
            deferred_enclosures: vec![],
            unknown_enclosures: vec![],
            submitted_meta: Default::default(),
        },
        broken: Vec::new(),
        deferred: Vec::new(),
        select_deferred: Vec::new(),
        unknown: Vec::new(),
        submitted_meta: Default::default(),
    };

    let outcome = subscribe::collect_completed_recorded(
        added,
        &Probe,
        &[],
        &library::WorkControl::default(),
        Some(&recorder),
    )
    .unwrap();
    assert!(!transfer_gate_called.load(std::sync::atomic::Ordering::SeqCst));
    assert!(marker.exists());
    assert_eq!(outcome.files.len(), 1);
    assert_eq!(outcome.files[0].file.path, renamed_dest);
}

struct Recorder<'a>(&'a Path);
impl subscribe::TransferRecorder for Recorder<'_> {
    fn prepare(
        &self,
        candidate: &domain::FileCoverage,
        _: &Path,
        _: TransferMode,
    ) -> Result<domain::FileCoverage, subscribe::SubscribeError> {
        std::fs::write(self.0, b"reserved")?;
        Ok(candidate.clone())
    }
    fn published(&self, _: &domain::FileCoverage) -> Result<(), subscribe::SubscribeError> {
        Ok(())
    }
}

fn media_fixture() -> Media {
    Media {
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
    }
}

fn filter_fixture() -> Filter {
    Filter {
        id: FilterId::new(),
        name: "default".into(),
        atoms: vec![FilterAtom {
            priority: 1,
            rule: AtomRule::Resolution("1080p".into()),
            exclude: false,
        }],
        keep_old_versions: false,
    }
}

fn subscribe_fixture(media: &Media, filter: &Filter) -> Subscribe {
    Subscribe {
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
    }
}

fn torrent_fixture() -> Torrent {
    Torrent {
        site_id: domain::SiteId::new(),
        title: "The Expanse S01E01-E02 1080p".into(),
        enclosure: "https://pt.example/expanse".into(),
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
        free_2x: false,
        free_until: None,
    }
}

struct PartialClaimableDownloader {
    files: Vec<PathBuf>,
    done: bool,
    claimed: bool,
}

impl Downloader for PartialClaimableDownloader {
    fn submit_verified(
        &self,
        payload: &downloader::SubmitPayload,
        _options: &downloader::SubmitOptions,
    ) -> Result<downloader::ClientSubmitResult, DownloaderError> {
        Ok(downloader::ClientSubmitResult::Added {
            already_owned: false,
            info_hash: payload.info_hash().to_string(),
        })
    }
    fn completed_files(&self, _: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        Ok(if self.done {
            self.files.clone()
        } else {
            Vec::new()
        })
    }
    fn collectable_files(&self, _: &Torrent) -> Result<(Vec<PathBuf>, bool), DownloaderError> {
        Ok((self.files.clone(), self.done))
    }
    fn is_claimed(&self, _: &Torrent) -> Result<bool, DownloaderError> {
        Ok(self.claimed)
    }
}

#[test]
fn plan_torrent_destinations_includes_claimed_partial_download_under_move() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_fixture();
    let filter = filter_fixture();
    let subscribe = subscribe_fixture(&media, &filter);
    let ep1 = tmp.path().join("The.Expanse.S01E01.mkv");
    std::fs::write(&ep1, b"video").unwrap();
    let torrent = torrent_fixture();
    let scored = ScoredTorrent {
        release: release::parse(&torrent.title),
        torrent,
        score: 80,
    };
    let lib_root = tmp.path().join("library");

    let unclaimed_dl = PartialClaimableDownloader {
        files: vec![ep1.clone()],
        done: false,
        claimed: false,
    };
    let unclaimed_plan = subscribe::plan_torrent_destinations(
        &subscribe,
        &media,
        &lib_root,
        None,
        Some(TransferMode::Move),
        None,
        &unclaimed_dl,
        &scored,
        &[],
    );
    assert!(
        unclaimed_plan.is_empty(),
        "unclaimed partial download under Move must wait until done"
    );

    let claimed_dl = PartialClaimableDownloader {
        files: vec![ep1.clone()],
        done: false,
        claimed: true,
    };
    let claimed_plan = subscribe::plan_torrent_destinations(
        &subscribe,
        &media,
        &lib_root,
        None,
        Some(TransferMode::Move),
        None,
        &claimed_dl,
        &scored,
        &[],
    );
    assert_eq!(
        claimed_plan.len(),
        1,
        "claimed partial download degrades Move -> Hardlink and must plan rename destinations"
    );
    assert_eq!(claimed_plan[0].0, ep1);
}
