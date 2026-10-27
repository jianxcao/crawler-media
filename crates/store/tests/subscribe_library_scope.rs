use std::path::{Path, PathBuf};
use std::str::FromStr;

use domain::{
    Confidence, Coverage, FetchMode, FilterId, LedgerId, LibraryId, Media, MediaId, MediaKind,
    QualitySource, Subscribe, SubscribeId, UserId,
};
use store::test_support::FixtureFile;
use store::{Library, Store};
use subscribe::{QualityFact, SubscribeFacts};

struct Fixture {
    tmp: tempfile::TempDir,
    store: Store,
    subscribe: Subscribe,
}

#[test]
fn snapshot_freshness_detects_saved_facts_and_other_library_changes() {
    let mut fixture = Fixture::new();
    let root = fixture.root("target");
    let target = fixture.library("Target", &root);
    fixture.select(&target);
    let snapshot = fixture
        .store
        .subscribe_library_snapshot(&fixture.subscribe, MediaKind::Movie)
        .unwrap();
    assert!(
        fixture
            .store
            .subscribe_snapshot_is_current(&snapshot, &Default::default())
            .unwrap()
    );

    fixture.save(&root.join("movie.mkv"), 20);
    assert!(
        !fixture
            .store
            .subscribe_snapshot_is_current(&snapshot, &Default::default())
            .unwrap()
    );

    let snapshot = fixture
        .store
        .subscribe_library_snapshot(&fixture.subscribe, MediaKind::Movie)
        .unwrap();
    fixture.library("Nested", &root.join("nested"));
    assert!(
        !fixture
            .store
            .subscribe_snapshot_is_current(&snapshot, &Default::default())
            .unwrap()
    );
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().join("data")).unwrap();
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
        let row = FixtureFile {
            file_id: Some(LedgerId::new()),
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
            ..Default::default()
        };
        store::test_support::publish_fixture_file(&self.store, &row).unwrap();
    }

    fn publish_probed_movie(&self, target: &Library, file_path: &Path) -> LedgerId {
        let roots = self.store.list_library_roots().unwrap();
        let root_id = roots
            .into_iter()
            .find(|r| r.library_id == target.id.clone())
            .unwrap()
            .id;

        let file_id = LedgerId::new();
        let file = domain::LibraryFile {
            file_id,
            library_id: target.id.parse().unwrap(),
            root_id,
            media_id: self.subscribe.media_id,
            kind: MediaKind::Movie,
            path: file_path.to_path_buf(),
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
        self.store
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
        file_id
    }

    fn load(&self) -> SubscribeFacts {
        self.store
            .subscribe_library_snapshot(&self.subscribe, MediaKind::Movie)
            .and_then(|snapshot| store::resolve_subscribe_facts(&snapshot)?.evaluate(&|_| true))
            .unwrap()
    }
}

#[test]
fn subscribe_quality_retains_observed_sdr_until_file_content_changes() {
    use store::library_write::probe::{Observed, ProbePublication};
    use store::library_write::{FilePublication, WriteScope};

    let mut f = Fixture::new();
    let root = f.root("target");
    let target = f.library("Target", &root);
    f.select(&target);
    let path = root.join("Movie.mkv");
    std::fs::write(&path, b"content").unwrap();
    let file_id = f.publish_probed_movie(&target, &path);
    let mut file = f.store.file_coverage_by_id(file_id).unwrap().unwrap().file;
    let path_str = path.to_str().unwrap().to_string();
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
            title: "Movie".into(),
            year: None,
            season: None,
            episode: None,
            episode_to: None,
            resolution: None,
            source: None,
            codec: None,
            hdr: Some("HDR10".into()),
            subtitle_language: None,
            audio_language: None,
            group: None,
            confidence: Confidence::High,
            season_inferred: false,
        },
    );
    f.store
        .save_subscribe_facts(f.subscribe.id, &facts)
        .unwrap();
    let version = file.version.clone();
    let probe = |hdr| ProbePublication {
        expected: &version,
        tracks_version: 1,
        resolution: None,
        codec: None,
        hdr,
        duration_ms: None,
        details: None,
    };
    f.store
        .publish_probe_result(file_id, &probe(Observed::NotObserved), 1)
        .unwrap();
    assert_eq!(
        f.load().quality(&path_str).unwrap().hdr.as_deref(),
        Some("HDR10")
    );
    f.store
        .publish_probe_result(file_id, &probe(Observed::Value(None)), 2)
        .unwrap();
    assert_eq!(f.load().quality(&path_str).unwrap().hdr, None);
    f.store
        .save_subscribe_facts(f.subscribe.id, &facts)
        .unwrap();
    assert_eq!(f.load().quality(&path_str).unwrap().hdr, None);

    let scope = WriteScope {
        library_id: file.library_id,
        config_revision: 0,
    };
    f.store
        .publish_files(
            &scope,
            &[FilePublication {
                file: &file,
                units: &[domain::UnitKey::WHOLE],
                expected_version: Some(&version),
                nfo_observation: None,
                probe_required: false,
            }],
            3,
        )
        .unwrap();
    assert_eq!(f.load().quality(&path_str).unwrap().hdr, None);

    file.version.source_version = "v2".into();
    file.hdr = None;
    f.store
        .publish_files(
            &scope,
            &[FilePublication {
                file: &file,
                units: &[domain::UnitKey::WHOLE],
                expected_version: Some(&version),
                nfo_observation: None,
                probe_required: false,
            }],
            4,
        )
        .unwrap();
    assert_eq!(
        f.load().quality(&path_str).unwrap().hdr.as_deref(),
        Some("HDR10")
    );
}

#[test]
fn replacing_file_source_discards_previous_hdr_observation() {
    use store::library_write::WriteScope;
    use store::library_write::probe::{Observed, ProbePublication};

    let mut f = Fixture::new();
    let root = f.root("target");
    let target = f.library("Target", &root);
    f.select(&target);
    let path = root.join("Movie.mkv");
    std::fs::write(&path, b"content").unwrap();
    let file_id = f.publish_probed_movie(&target, &path);
    let file = f.store.file_coverage_by_id(file_id).unwrap().unwrap().file;
    let path_str = path.to_str().unwrap().to_string();
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
            title: "Movie".into(),
            year: None,
            season: None,
            episode: None,
            episode_to: None,
            resolution: None,
            source: None,
            codec: None,
            hdr: Some("HDR10".into()),
            subtitle_language: None,
            audio_language: None,
            group: None,
            confidence: Confidence::High,
            season_inferred: false,
        },
    );
    f.store
        .save_subscribe_facts(f.subscribe.id, &facts)
        .unwrap();
    f.store
        .publish_probe_result(
            file_id,
            &ProbePublication {
                expected: &file.version,
                tracks_version: 1,
                resolution: None,
                codec: None,
                hdr: Observed::Value(None),
                duration_ms: None,
                details: None,
            },
            1,
        )
        .unwrap();
    assert_eq!(f.load().quality(&path_str).unwrap().hdr, None);
    f.store
        .replace_file_source(
            &WriteScope {
                library_id: file.library_id,
                config_revision: 0,
            },
            file_id,
            &file.version,
            &library::SourceObservation {
                source_version: "v2".into(),
                size_bytes: None,
                modified_ns: None,
                created_ns: None,
                content_digest: None,
            },
            2,
        )
        .unwrap();
    assert_eq!(
        f.load().quality(&path_str).unwrap().hdr.as_deref(),
        Some("HDR10")
    );
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
    f.library("Other", f.tmp.path());
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
    std::fs::create_dir_all(&broken).unwrap();
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
    // The root becomes unreachable after valid configuration was recorded.
    std::fs::remove_dir(&broken).unwrap();
    std::os::unix::fs::symlink(f.root("never-created"), &broken).unwrap();

    assert!(
        f.store
            .subscribe_library_snapshot(&f.subscribe, MediaKind::Movie)
            .and_then(|snapshot| store::resolve_subscribe_facts(&snapshot)?.evaluate(&|_| true))
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
            .subscribe_library_snapshot(&f.subscribe, MediaKind::Movie)
            .and_then(|snapshot| store::resolve_subscribe_facts(&snapshot)?.evaluate(&|_| true))
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

    f.publish_probed_movie(&target, &file_path);

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

    f.store
        .save_subscribe_facts(f.subscribe.id, &facts)
        .unwrap();

    // Load the library subscribe facts back
    let loaded = f.load();
    assert_eq!(
        loaded.movie().unwrap().path.as_deref(),
        Some(path_str.as_str())
    );
    let quality = loaded
        .quality(&path_str)
        .expect("quality must be read back");

    // Probe fields on the file row (2160p, hevc, HDR10, High) must overlay release dimensions
    assert_eq!(quality.resolution.as_deref(), Some("2160p"));
    assert_eq!(quality.codec.as_deref(), Some("hevc"));
    assert_eq!(quality.hdr.as_deref(), Some("HDR10"));
    assert_eq!(quality.confidence, Confidence::High);
    // Stored release source dimension must be preserved
    assert_eq!(quality.source.as_deref(), Some("WEB-DL"));
}

#[test]
fn removing_explicit_library_binding_does_not_jump_to_default_library_facts() {
    let mut f = Fixture::new();
    let def_root = f.root("default_lib");
    let def_lib = f.library("Default", &def_root);
    f.store.set_default_library(&def_lib.id).unwrap();

    let other_root = f.root("other_lib");
    let other_lib = f.library("Other", &other_root);

    // Bind to other_lib first
    f.select(&other_lib);
    let other_file = other_root.join("Movie.mkv");
    f.ledger(&other_file, 100);
    f.save(&other_file, 100);

    // Facts are visible under other_lib
    let facts = f.load();
    assert!(facts.movie().is_some());

    // Now remove explicit binding: subscribe.library_id = None
    f.subscribe.library_id = None;
    f.store.update_subscribe(&f.subscribe).unwrap();

    // With explicit binding removed, the facts saved under other_lib should NOT
    // be claimed as belonging to default_lib
    let facts_after = f.load();
    assert!(
        facts_after.movie().is_none(),
        "Saved facts of another library must not jump to default library"
    );
}

#[cfg(unix)]
#[test]
fn recorded_facts_do_not_resolve_unrelated_library_roots() {
    let mut f = Fixture::new();
    let root = f.root("recorded");
    let target = f.library("Recorded", &root);
    f.select(&target);
    let path = root.join("Scoped.mkv");
    f.ledger(&path, 100);
    let broken = f.root("broken-unrelated");
    f.library("Unrelated", &broken);
    let snapshot = f
        .store
        .subscribe_library_snapshot(&f.subscribe, MediaKind::Movie)
        .unwrap();
    std::fs::remove_dir(&broken).unwrap();
    std::os::unix::fs::symlink(f.root("absent"), &broken).unwrap();
    let facts = store::resolve_subscribe_facts(&snapshot)
        .and_then(|resolved| resolved.evaluate(&|_| true))
        .unwrap();
    assert!(facts.movie().is_some());
}

#[cfg(unix)]
#[test]
fn resolved_saved_facts_evaluate_without_reopening_the_root() {
    let mut f = Fixture::new();
    let root = f.root("saved-root");
    let target = f.library("Saved", &root);
    f.select(&target);
    f.save(&root.join("missing.mkv"), 100);
    let snapshot = f
        .store
        .subscribe_library_snapshot(&f.subscribe, MediaKind::Movie)
        .unwrap();
    let resolved = store::resolve_subscribe_facts(&snapshot).unwrap();
    std::fs::remove_dir(&root).unwrap();
    std::os::unix::fs::symlink(f.root("absent"), &root).unwrap();
    assert!(resolved.evaluate(&|_| true).unwrap().movie().is_some());
}
