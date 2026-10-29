//! Task 3 RED draft: public page-source validation and executed-query work.
use domain::{LedgerId, LibraryFile, LibraryId, Media, MediaId, MediaKind};
use store::library_write::WriteScope;
use store::stats::StatsScope;
use store::{PageSnapshot, PageValidation, Store};

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    library: LibraryId,
    files: Vec<LibraryFile>,
}

impl Fixture {
    fn new(count: usize) -> Self {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let root = dir.path().join("movies");
        std::fs::create_dir_all(&root).unwrap();
        let library = store
            .create_library(
                MediaKind::Movie,
                "Page validation",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
            .id
            .parse()
            .unwrap();
        let files = (0..count)
            .map(|index| {
                let media = Media {
                    id: MediaId::new(),
                    kind: MediaKind::Movie,
                    title: format!("Movie {index:03}"),
                    year: Some(2024),
                    original_title: None,
                    aliases: Vec::new(),
                    tmdb_id: None,
                    douban_id: None,
                    tvdb_id: None,
                    bangumi_id: None,
                    anilist_id: None,
                };
                store.insert_media(&media).unwrap();
                let path = root.join(format!("{index:03}.mkv"));
                std::fs::write(&path, b"local page validation fixture").unwrap();
                store::test_support::publish_fixture_file(
                    &store,
                    &store::test_support::FixtureFile {
                        media_id: media.id,
                        path: path.display().to_string(),
                        ..Default::default()
                    },
                )
                .unwrap()
            })
            .collect();
        Self {
            _dir: dir,
            store,
            library,
            files,
        }
    }

    fn snapshot(&self, count: usize) -> PageSnapshot {
        PageSnapshot {
            config_revision: self.store.metadata_language().unwrap().revision,
            files: self
                .files
                .iter()
                .take(count)
                .map(|file| {
                    (
                        file.file_id,
                        file.version.revision,
                        file.media_id,
                        file.library_id,
                    )
                })
                .collect(),
        }
    }

    fn revise_file(&self, index: usize) -> LibraryFile {
        let changed = &self.files[index];
        std::fs::write(&changed.path, b"replacement bytes with a different length").unwrap();
        let observation = library::observe_source(&changed.path).unwrap();
        let scope = WriteScope {
            library_id: self.library,
            config_revision: self
                .store
                .library_config_revision(&self.library.to_string())
                .unwrap(),
        };
        self.store
            .replace_file_source(
                &scope,
                changed.file_id,
                &changed.version,
                &observation,
                1000,
            )
            .unwrap();
        self.store
            .library_file_page(self.library, changed.media_id, None, 1)
            .unwrap()
            .files
            .remove(0)
    }
}

#[test]
fn unchanged_page_snapshot_is_fresh() {
    let fixture = Fixture::new(3);
    let snapshot = fixture.snapshot(3);
    assert!(matches!(
        fixture.store.validate_page_snapshot(&snapshot).unwrap(),
        PageValidation::Fresh
    ));
}

#[test]
fn validate_reports_files_removed_after_prepare() {
    let fixture = Fixture::new(3);
    let snapshot = fixture.snapshot(3);
    let removed = fixture.files[1].file_id;
    assert_eq!(
        fixture
            .store
            .delete_files(fixture.library, &[removed])
            .unwrap(),
        1
    );
    match fixture.store.validate_page_snapshot(&snapshot).unwrap() {
        PageValidation::Stale { gone } => assert_eq!(gone, vec![removed]),
        PageValidation::Fresh => panic!("a removed prepared source cannot be fresh"),
    }
}

#[test]
fn validate_reports_changed_source_revision_without_deleting_the_file() {
    let fixture = Fixture::new(3);
    let snapshot = fixture.snapshot(3);
    let changed = fixture.revise_file(1);
    assert_eq!(changed.file_id, fixture.files[1].file_id);
    assert!(changed.version.revision > fixture.files[1].version.revision);
    match fixture.store.validate_page_snapshot(&snapshot).unwrap() {
        PageValidation::Stale { gone } => assert_eq!(gone, vec![changed.file_id]),
        PageValidation::Fresh => panic!("an old prepared revision cannot be fresh"),
    }
}

#[test]
fn validate_reports_query_config_revision_change_without_inventing_gone_files() {
    let fixture = Fixture::new(3);
    let mut snapshot = fixture.snapshot(3);
    // This token represents a prepare from a different query-config revision;
    // avoid triggering a real locale rebuild merely to test token validation.
    snapshot.config_revision += 1;
    match fixture.store.validate_page_snapshot(&snapshot).unwrap() {
        PageValidation::Stale { gone } => assert!(gone.is_empty()),
        PageValidation::Fresh => panic!("a mismatched query-config token cannot be fresh"),
    }
}

#[test]
fn page_validate_and_hydration_have_no_full_scan() {
    let fixture = Fixture::new(80);
    let snapshot = fixture.snapshot(3);
    let ids: Vec<LedgerId> = snapshot.files.iter().map(|row| row.0).collect();
    let (validation, validation_work) = fixture.store.with_stats(StatsScope::Library, || {
        fixture.store.validate_page_snapshot(&snapshot)
    });
    assert!(matches!(validation.unwrap(), PageValidation::Fresh));
    assert_eq!(validation_work.fullscan_steps, 0, "{validation_work:?}");
    assert_eq!(validation_work.sort_operations, 0, "{validation_work:?}");
    let (hydrated, hydration_work) = fixture
        .store
        .with_stats(StatsScope::Library, || fixture.store.page_hydration(&ids));
    let hydrated = hydrated.unwrap();
    assert_eq!(hydrated.len(), ids.len());
    assert!(ids.iter().all(|file| hydrated.contains_key(file)));
    assert_eq!(hydration_work.fullscan_steps, 0, "{hydration_work:?}");
    assert_eq!(hydration_work.sort_operations, 0, "{hydration_work:?}");
}

#[test]
fn page_validate_and_hydration_batch_statement_counts_do_not_grow_per_file() {
    let fixture = Fixture::new(10);
    let one = fixture.snapshot(1);
    let many = fixture.snapshot(10);
    let (_, one_validation) = fixture.store.with_stats(StatsScope::Library, || {
        fixture.store.validate_page_snapshot(&one).unwrap()
    });
    let (_, many_validation) = fixture.store.with_stats(StatsScope::Library, || {
        fixture.store.validate_page_snapshot(&many).unwrap()
    });
    assert_eq!(one_validation.statements, many_validation.statements);
    let one_id = vec![fixture.files[0].file_id];
    let many_ids: Vec<LedgerId> = fixture.files.iter().map(|file| file.file_id).collect();
    let (_, one_hydration) = fixture.store.with_stats(StatsScope::Library, || {
        let result = fixture.store.page_hydration(&one_id).unwrap();
        assert_eq!(result.len(), 1);
    });
    let (_, many_hydration) = fixture.store.with_stats(StatsScope::Library, || {
        let result = fixture.store.page_hydration(&many_ids).unwrap();
        assert_eq!(result.len(), 10);
    });
    assert_eq!(one_hydration.statements, many_hydration.statements);
}

#[test]
fn actual_page_read_query_plans_use_indexes() {
    let fixture = Fixture::new(80);
    let plans = fixture.store.page_read_query_plans().unwrap();
    assert!(!plans.is_empty());
    for plan in plans {
        let upper = plan.to_ascii_uppercase();
        assert!(!upper.contains("TEMP B-TREE"), "{plan}");
        if upper.contains("SCAN ") {
            assert!(
                upper.contains("VIRTUAL TABLE"),
                "only selected JSON input may be scanned: {plan}"
            );
        }
    }
}

#[test]
fn page_hydration_keeps_current_tracks_chapters_and_default_marker() {
    let fixture = Fixture::new(2);
    let file = &fixture.files[0];
    let tracks = library::Tracks {
        video: Some(library::VideoTrack {
            width: Some(1920),
            height: Some(1080),
            ..Default::default()
        }),
        audio: vec![library::AudioTrack {
            codec: Some("aac".into()),
            ..Default::default()
        }],
        subtitles: vec![],
    };
    let chapters = vec![library::ChapterMarker {
        start_ms: 100,
        end_ms: 300,
        title: Some("Intro".into()),
        marker_type: None,
        synthetic: false,
    }];
    fixture
        .store
        .put_file_meta(&file.file_id.to_string(), &tracks)
        .unwrap();
    fixture
        .store
        .put_cached_chapters(&file.file_id.to_string(), &chapters)
        .unwrap();
    for episode in [1, 2] {
        fixture
            .store
            .put_media_marker(&store::StoredMediaMarker {
                media_id: file.media_id,
                season: 1,
                episode,
                intro_start_ms: Some(episode as i64 * 10),
                intro_end_ms: Some(100),
                outro_start_ms: None,
                outro_end_ms: None,
                source: "manual".into(),
                locked: false,
                updated_at: 1,
            })
            .unwrap();
    }
    let hydrated = fixture
        .store
        .page_hydration(&[file.file_id, fixture.files[1].file_id])
        .unwrap();
    assert_eq!(hydrated[&file.file_id].tracks, tracks);
    assert_eq!(hydrated[&file.file_id].chapters, chapters);
    assert_eq!(
        hydrated[&file.file_id].markers[&(1, 1)].intro_start_ms,
        Some(10)
    );
    assert!(!hydrated[&file.file_id].markers.contains_key(&(1, 2)));
    assert!(hydrated[&fixture.files[1].file_id].markers.is_empty());
    let revised = fixture.revise_file(0);
    let current = fixture.store.page_hydration(&[revised.file_id]).unwrap();
    assert_eq!(current[&revised.file_id].tracks, library::Tracks::default());
    assert!(current[&revised.file_id].chapters.is_empty());
}
