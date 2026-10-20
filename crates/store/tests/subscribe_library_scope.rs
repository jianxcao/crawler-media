use std::path::{Path, PathBuf};
use std::str::FromStr;

use domain::{
    Confidence, Coverage, FetchMode, FilterId, LedgerId, LedgerRow, LibraryId, Media, MediaId,
    MediaKind, QualitySource, Subscribe, SubscribeId, UserId,
};
use store::{Library, Store};
use subscribe::{QualityFact, SubscribeFacts};

struct Fixture {
    tmp: tempfile::TempDir,
    store: Store,
    subscribe: Subscribe,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("data")).unwrap();
        let media_id = MediaId::new();
        store
            .insert_media(&Media {
                id: media_id,
                kind: MediaKind::Movie,
                title: "Scoped".into(),
                year: None,
                original_title: None,
                aliases: Vec::new(),
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        let subscribe = Subscribe {
            id: SubscribeId::new(),
            user_id: UserId::new(),
            media_id,
            coverage: Coverage::Movie,
            fetch_mode: FetchMode::Search,
            filter_id: FilterId::new(),
            wash_cut: false,
            wash_cut_filter_id: None,
            keep_old_versions: false,
            full_season_pack: false,
            downloader_id: None,
            library_id: None,
            tracking_state: "active".into(),
            follow_future: false,
            search_interval_secs: 1800,
        };
        Self {
            tmp,
            store,
            subscribe,
        }
    }

    fn root(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }

    fn library(&self, name: &str, root: &Path) -> Library {
        std::fs::create_dir_all(root).unwrap();
        self.store
            .create_library(
                MediaKind::Movie,
                name,
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
    }

    fn select(&mut self, library: &Library) {
        self.subscribe.library_id = Some(LibraryId::from_str(&library.id).unwrap());
    }

    fn save(&self, path: &Path, score: i32) {
        let mut facts = SubscribeFacts::default();
        facts.replace(
            None,
            None,
            QualityFact {
                score,
                path: Some(path.to_str().unwrap().into()),
            },
        );
        self.store
            .save_subscribe_facts(self.subscribe.id, &facts)
            .unwrap();
    }

    fn ledger(&self, path: &Path, score: i32) {
        self.store
            .insert_ledger(&LedgerRow {
                id: LedgerId::new(),
                media_id: self.subscribe.media_id,
                path: path.to_str().unwrap().into(),
                season: None,
                episode: None,
                resolution: Some("2160p".into()),
                codec: Some("hevc".into()),
                hdr: None,
                quality_source: QualitySource::Probe,
                confidence: Confidence::High,
                filter_score: Some(score),
            })
            .unwrap();
    }

    fn load(&self) -> SubscribeFacts {
        self.store
            .load_library_subscribe_facts(&self.subscribe, MediaKind::Movie)
            .unwrap()
    }
}

#[test]
fn missing_previous_library_fact_does_not_fill_empty_retargeted_library() {
    let mut f = Fixture::new();
    let a_root = f.root("a");
    let a = f.library("A", &a_root);
    let b = f.library("B", &f.root("b"));
    let deleted = a_root.join("missing/sub/Scoped.mkv");
    f.select(&a);
    f.ledger(&deleted, 100);
    f.save(&deleted, 100);
    f.select(&b);

    let facts = f.load();
    assert!(facts.movie().is_none(), "A's missing file must not fill B");
    assert!(facts.quality(deleted.to_str().unwrap()).is_none());
    assert!(!deleted.exists());
    assert!(
        f.store
            .load_subscribe_facts(f.subscribe.id)
            .unwrap()
            .movie()
            .is_some(),
        "scoping must not rewrite saved deletion facts"
    );
}

#[test]
fn outside_existing_path_is_not_owned_even_by_default_target() {
    let mut f = Fixture::new();
    let target = f.library("Target", &f.root("target"));
    f.store.set_default_library(&target.id).unwrap();
    let external = f.root("external.mkv");
    std::fs::write(&external, b"external").unwrap();
    f.library("Other", &f.tmp.path().to_path_buf());
    f.ledger(&external, 100);
    f.save(&external, 100);
    // Resolve the selected Library through the default, not an explicit id.
    f.subscribe.library_id = None;

    let facts = f.load();
    assert!(facts.movie().is_none());
    assert!(facts.quality(external.to_str().unwrap()).is_none());
    assert!(external.is_file());
}

#[test]
fn outside_missing_and_relative_basename_facts_are_unscoped() {
    let mut f = Fixture::new();
    let target = f.library("Target", &f.root("target"));
    f.select(&target);
    for path in [
        f.root("missing/outside.mkv"),
        PathBuf::from("unscoped-saved-fact.mkv"),
    ] {
        f.save(&path, 100);
        assert!(
            f.load().movie().is_none(),
            "unscoped path: {}",
            path.display()
        );
    }
}

#[test]
fn same_library_deletion_fact_and_quality_remain_owned() {
    let mut f = Fixture::new();
    let root = f.root("target");
    let target = f.library("Target", &root);
    f.select(&target);
    let deleted = root.join("deleted/sub/Scoped.mkv");
    f.ledger(&deleted, 80);
    f.save(&deleted, 80);

    let facts = f.load();
    assert_eq!(facts.movie().unwrap().score, 80);
    assert_eq!(facts.movie().unwrap().path.as_deref(), deleted.to_str());
    assert_eq!(
        facts
            .quality(deleted.to_str().unwrap())
            .unwrap()
            .resolution
            .as_deref(),
        Some("2160p")
    );
    assert!(
        !deleted.exists(),
        "loading deletion facts must not recreate files"
    );
}

#[test]
fn nested_library_owns_missing_fact_instead_of_parent() {
    let mut f = Fixture::new();
    let root = f.root("parent");
    let parent = f.library("Parent", &root);
    let child_root = root.join("child");
    let child = f.library("Child", &child_root);
    let deleted = child_root.join("missing/Scoped.mkv");
    f.save(&deleted, 90);
    f.select(&parent);
    assert!(f.load().movie().is_none());
    f.select(&child);
    assert_eq!(f.load().movie().unwrap().path.as_deref(), deleted.to_str());
}

#[test]
fn retained_target_ledger_fact_replaces_foreign_fact_and_quality() {
    let mut f = Fixture::new();
    let foreign_root = f.root("foreign");
    f.library("Foreign", &foreign_root);
    let target_root = f.root("target");
    let target = f.library("Target", &target_root);
    f.select(&target);
    let foreign = foreign_root.join("deleted.mkv");
    let owned = target_root.join("Scoped.mkv");
    std::fs::write(&owned, b"owned").unwrap();
    f.ledger(&foreign, 100);
    f.ledger(&owned, 50);
    f.save(&foreign, 100);

    let facts = f.load();
    assert_eq!(facts.movie().unwrap().path.as_deref(), owned.to_str());
    assert_eq!(facts.movie().unwrap().score, 50);
    assert!(facts.quality(foreign.to_str().unwrap()).is_none());
    assert!(facts.quality(owned.to_str().unwrap()).is_some());
}

#[cfg(unix)]
#[test]
fn root_resolution_failure_is_returned_not_treated_as_ownership() {
    let mut f = Fixture::new();
    let root = f.root("target");
    let target = f.library("Target", &root);
    f.select(&target);
    let path = root.join("Scoped.mkv");
    std::fs::write(&path, b"owned").unwrap();
    f.save(&path, 80);
    // A dangling root gives deterministic IO failure, including when run as root.
    let broken = f.root("broken");
    std::os::unix::fs::symlink(f.root("never-created"), &broken).unwrap();
    f.store
        .create_library(
            MediaKind::Movie,
            "Broken",
            &[broken.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    assert!(
        f.store
            .load_library_subscribe_facts(&f.subscribe, MediaKind::Movie)
            .is_err(),
        "strict root lookup failure must propagate, not retain the saved fact"
    );
}

#[cfg(unix)]
#[test]
fn symlink_escape_is_not_owned_or_reintroduced_from_ledger() {
    let mut f = Fixture::new();
    let root = f.root("target");
    let target = f.library("Target", &root);
    f.select(&target);
    let outside = f.root("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("Scoped.mkv"), b"external").unwrap();
    f.library("Outside", &outside);
    f.ledger(&outside.join("Scoped.mkv"), 100);
    std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
    let path = root.join("escape/Scoped.mkv");
    f.save(&path, 100);

    let facts = f.load();
    assert!(facts.movie().is_none());
    assert!(facts.quality(path.to_str().unwrap()).is_none());
    assert!(path.is_file());
}

#[test]
fn library_database_lookup_failure_is_returned() {
    let mut f = Fixture::new();
    let target = f.library("Target", &f.root("target"));
    f.select(&target);
    f.save(&f.root("target/missing.mkv"), 80);
    let db = rusqlite::Connection::open(f.tmp.path().join("data/library.db")).unwrap();
    db.execute_batch("DROP TABLE library_roots").unwrap();

    assert!(
        f.store
            .load_library_subscribe_facts(&f.subscribe, MediaKind::Movie)
            .is_err()
    );
}

#[test]
fn qualities_follow_retained_facts_after_ledger_score_selection() {
    let mut f = Fixture::new();
    let root = f.root("target");
    let target = f.library("Target", &root);
    f.select(&target);
    let deleted = root.join("deleted.mkv");
    let lower = root.join("lower.mkv");
    std::fs::write(&lower, b"lower").unwrap();
    f.ledger(&deleted, 100);
    f.ledger(&lower, 50);
    f.save(&deleted, 100);

    let facts = f.load();
    assert_eq!(facts.movie().unwrap().path.as_deref(), deleted.to_str());
    assert!(facts.quality(deleted.to_str().unwrap()).is_some());
    assert!(
        facts.quality(lower.to_str().unwrap()).is_none(),
        "a rejected ledger candidate must not leave detached quality"
    );
}

#[test]
fn subscribe_quality_evaluation_overlays_probe_fields_for_published_file() {
    let mut f = Fixture::new();
    let root = f.root("target");
    let target = f.library("Target", &root);
    f.select(&target);

    let file_path = root.join("Movie.mkv");
    let path_str = file_path.to_str().unwrap().to_string();
    std::fs::write(&file_path, b"content").unwrap();

    let roots = f.store.list_library_roots().unwrap();
    let root_id = roots
        .into_iter()
        .find(|r| r.library_id == Some(target.id.clone()))
        .unwrap()
        .id;

    let file_id = LedgerId::new();
    let file = domain::LibraryFile {
        file_id,
        library_id: target.id.parse().unwrap(),
        root_id,
        media_id: f.subscribe.media_id,
        kind: MediaKind::Movie,
        path: file_path.clone(),
        version: domain::FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: domain::FilePresence::Present,
        publication: domain::FilePublication::Published,
        size_bytes: Some(1_000_000),
        stat_known: true,
        created_ns: None,
        modified_ns: None,
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: Some("2160p".into()),
        codec: Some("hevc".into()),
        hdr: Some("HDR10".into()),
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: Some(100),
        release_quality: None,
        duration_ms: Some(7_200_000),
        tracks_version: 1,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    };

    let scope = store::library_write::WriteScope {
        library_id: target.id.parse().unwrap(),
        config_revision: 0,
    };
    f.store
        .publish_files(
            &scope,
            &[store::library_write::FilePublication {
                file: &file,
                units: &[domain::UnitKey::WHOLE],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1000,
        )
        .unwrap();

    // Record quality in SubscribeFacts with release dimensions
    let mut facts = SubscribeFacts::default();
    facts.replace(
        None,
        None,
        QualityFact {
            score: 95,
            path: Some(path_str.clone()),
        },
    );
    facts.set_quality(
        path_str.clone(),
        domain::Release {
            title: "Test".into(),
            year: None,
            season: None,
            episode: None,
            episode_to: None,
            resolution: Some("1080p".into()),
            source: Some("WEB-DL".into()),
            codec: Some("h264".into()),
            hdr: None,
            subtitle_language: None,
            audio_language: None,
            group: None,
            confidence: Confidence::Low,
            season_inferred: false,
        },
    );

    f.store.save_subscribe_facts(f.subscribe.id, &facts).unwrap();

    // Load the library subscribe facts back
    let loaded = f.load();
    assert_eq!(loaded.movie().unwrap().path.as_deref(), Some(path_str.as_str()));
    let quality = loaded.quality(&path_str).expect("quality must be read back");

    // Probe fields on the file row (2160p, hevc, HDR10, High) must overlay release dimensions
    assert_eq!(quality.resolution.as_deref(), Some("2160p"));
    assert_eq!(quality.codec.as_deref(), Some("hevc"));
    assert_eq!(quality.hdr.as_deref(), Some("HDR10"));
    assert_eq!(quality.confidence, Confidence::High);
    // Stored release source dimension must be preserved
    assert_eq!(quality.source.as_deref(), Some("WEB-DL"));
}
