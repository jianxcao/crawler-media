//! Screenshot obligations commit with the published file-local chapter facts.
use domain::{FileCoverage, LibraryFile, Media, MediaId, MediaKind, UnitKey};
use store::test_support::{FixtureFile, publish_fixture_file_units};
use store::{MarkerResultReplacement, SourceObservation, Store, StoredFileMarker};

struct Fixture {
    _temp: tempfile::TempDir,
    store: Store,
    file: LibraryFile,
}

impl Fixture {
    fn new(units: &[UnitKey]) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("data")).unwrap();
        let root = temp.path().join("media");
        std::fs::create_dir(&root).unwrap();
        store
            .create_library(
                MediaKind::Tv,
                "Episodes",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "Episodes".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&media).unwrap();
        let path = root.join("episode.mkv");
        std::fs::write(&path, b"registered").unwrap();
        let file = publish_fixture_file_units(
            &store,
            &FixtureFile {
                media_id: media.id,
                path: path.display().to_string(),
                ..Default::default()
            },
            units,
        )
        .unwrap();
        Self {
            _temp: temp,
            store,
            file,
        }
    }

    fn row(&self) -> FileCoverage {
        self.store
            .file_coverage_by_id(self.file.file_id)
            .unwrap()
            .unwrap()
    }
    fn members(&self) -> Vec<SourceObservation> {
        vec![SourceObservation {
            file_id: self.file.file_id,
            expected: self.file.version.clone(),
            presence: domain::FilePresence::Present,
        }]
    }
    fn marker(&self, unit: UnitKey, start: i64) -> StoredFileMarker {
        StoredFileMarker {
            file_id: self.file.file_id,
            version: self.file.version.clone(),
            unit,
            intro_start_ms: Some(start),
            intro_end_ms: Some(start + 10_000),
            outro_start_ms: None,
            outro_end_ms: None,
            source: "verified".into(),
            locked: true,
            updated_at: 0,
        }
    }
    fn targets(&self) -> Vec<(i64, i64)> {
        self.store
            .current_chapter_frame_task(self.file.file_id)
            .unwrap()
            .expect("chapter publication must persist a screenshot obligation")
            .task
            .targets
    }
}

fn chapter(start: i64, end: i64) -> library::ChapterMarker {
    library::ChapterMarker {
        start_ms: start,
        end_ms: end,
        title: None,
        marker_type: None,
        synthetic: false,
    }
}

#[test]
fn cached_chapters_and_checked_markers_persist_obligations_without_a_dispatcher() {
    let f = Fixture::new(&[UnitKey::new(1, 1)]);
    f.store
        .put_cached_chapters(
            &f.file.file_id.to_string(),
            &[chapter(20_000, 30_000), chapter(0, 10_000)],
        )
        .unwrap();
    assert_eq!(f.targets(), vec![(0, 10_000), (20_000, 30_000)]);
    f.store
        .put_file_marker_checked(&f.marker(UnitKey::new(1, 1), 40_000), &f.members())
        .unwrap();
    assert!(f.targets().contains(&(40_000, 50_000)));
    f.store
        .put_cached_chapters(&f.file.file_id.to_string(), &[])
        .unwrap();
    // Current adopted markers still contribute after a physical chapter cache is cleared.
    assert!(f.targets().contains(&(40_000, 50_000)));
    f.store
        .delete_file_marker(f.file.file_id, UnitKey::new(1, 1))
        .unwrap();
    assert!(
        f.store
            .current_chapter_frame_task(f.file.file_id)
            .unwrap()
            .is_none()
    );
    assert!(f.store.pending_chapter_frame_tasks(10).unwrap().is_empty());
}

#[test]
fn merge_and_batch_replacement_publish_frames_for_the_committed_marker() {
    let f = Fixture::new(&[UnitKey::new(1, 1)]);
    let marker = f.marker(UnitKey::new(1, 1), 10_000);
    f.store
        .merge_marker_results_checked(&[f.row()], &f.members(), &[marker], false)
        .unwrap();
    assert!(f.targets().contains(&(10_000, 20_000)));
    // Caller-supplied derived chapter timelines are not authoritative physical chapters.
    let replacement = MarkerResultReplacement {
        media_id: f.file.media_id,
        season: 1,
        markers: vec![f.marker(UnitKey::new(1, 1), 60_000)],
        targets: vec![f.row()],
        chapter_updates: vec![(
            f.file.file_id.to_string(),
            vec![chapter(999_000, 1_000_000)],
        )],
    };
    f.store
        .replace_marker_results_batch_checked(&[replacement], &f.members())
        .unwrap();
    let targets = f.targets();
    assert!(targets.contains(&(60_000, 70_000)));
    assert!(!targets.contains(&(10_000, 20_000)));
    assert!(!targets.contains(&(999_000, 1_000_000)));
}

#[test]
fn failed_batch_rolls_back_both_markers_and_frame_obligations() {
    let f = Fixture::new(&[UnitKey::new(1, 1)]);
    let replacement = MarkerResultReplacement {
        media_id: f.file.media_id,
        season: 1,
        markers: vec![f.marker(UnitKey::new(1, 1), 10_000)],
        targets: vec![f.row()],
        chapter_updates: vec![(f.file.file_id.to_string(), vec![])],
    };
    let mut invalid = replacement.clone();
    invalid.markers[0].unit = UnitKey::new(1, 2);
    assert!(
        f.store
            .replace_marker_results_batch_checked(&[replacement, invalid], &f.members())
            .is_err()
    );
    assert!(
        f.store
            .get_file_marker(f.file.file_id, UnitKey::new(1, 1))
            .unwrap()
            .is_none()
    );
    assert!(f.store.pending_chapter_frame_tasks(10).unwrap().is_empty());
}

#[test]
fn updating_one_unit_keeps_the_other_units_current_targets_in_the_file_generation() {
    let f = Fixture::new(&[UnitKey::new(1, 1), UnitKey::new(1, 2)]);
    let first = f.marker(UnitKey::new(1, 1), 10_000);
    let second = f.marker(UnitKey::new(1, 2), 80_000);
    for marker in [&first, &second] {
        f.store
            .put_file_marker_checked(marker, &f.members())
            .unwrap();
    }
    let before = f.targets();
    assert!(before.contains(&(10_000, 20_000)) && before.contains(&(80_000, 90_000)));
    f.store
        .put_file_marker_checked(&f.marker(UnitKey::new(1, 1), 30_000), &f.members())
        .unwrap();
    let after = f.targets();
    assert!(after.contains(&(30_000, 40_000)) && after.contains(&(80_000, 90_000)));
    assert!(!after.contains(&(10_000, 20_000)));
}

#[test]
fn re_adopted_targets_allocate_a_fresh_obligation_without_reviving_old_attempts() {
    let f = Fixture::new(&[UnitKey::new(1, 1)]);
    let id = f.file.file_id.to_string();
    f.store
        .put_cached_chapters(&id, &[chapter(0, 10_000)])
        .unwrap();
    let a = f
        .store
        .current_chapter_frame_task(f.file.file_id)
        .unwrap()
        .unwrap();
    let claim = f
        .store
        .claim_chapter_frame_task(&a.task.id)
        .unwrap()
        .unwrap();
    f.store
        .finish_chapter_frame_attempt(
            &a.task.id,
            claim.attempt,
            &[store::ChapterFrameTargetResult {
                index: 0,
                ready: true,
                error: None,
            }],
        )
        .unwrap();
    let old = f
        .store
        .chapter_frame_task_snapshot(&a.task.id)
        .unwrap()
        .unwrap();
    f.store
        .put_cached_chapters(&id, &[chapter(20_000, 30_000)])
        .unwrap();
    let b = f
        .store
        .current_chapter_frame_task(f.file.file_id)
        .unwrap()
        .unwrap();
    f.store
        .put_cached_chapters(&id, &[chapter(0, 10_000)])
        .unwrap();
    let fresh = f
        .store
        .current_chapter_frame_task(f.file.file_id)
        .unwrap()
        .unwrap();
    assert_eq!(fresh.status.generation, a.status.generation);
    assert_ne!(fresh.task.id, a.task.id);
    assert_ne!(fresh.task.id, b.task.id);
    assert_eq!(fresh.task.targets, vec![(0, 10_000)]);
    assert_eq!(
        f.store
            .chapter_frame_task_status(&a.task.id)
            .unwrap()
            .unwrap()
            .state,
        store::ChapterFrameState::Superseded
    );
    assert_eq!(
        f.store
            .chapter_frame_task_status(&b.task.id)
            .unwrap()
            .unwrap()
            .state,
        store::ChapterFrameState::Superseded
    );
    assert!(
        !f.store
            .finish_chapter_frame_attempt(&a.task.id, claim.attempt, &[])
            .unwrap()
    );
    assert!(
        !f.store
            .repair_missing_chapter_frame_targets(
                &a.task.id,
                &a.status.generation,
                old.status.attempt,
                &[(0, old.targets[0].published.clone().unwrap())]
            )
            .unwrap()
            .applied
    );
    f.store
        .put_cached_chapters(&id, &[chapter(0, 10_000)])
        .unwrap();
    assert_eq!(
        f.store
            .current_chapter_frame_task(f.file.file_id)
            .unwrap()
            .unwrap()
            .task
            .id,
        fresh.task.id
    );
}

#[test]
fn refresh_job_completion_and_current_frames_commit_from_the_same_marker_facts() {
    let f = Fixture::new(&[UnitKey::new(1, 1)]);
    let id = f.file.file_id.to_string();
    let job = "chapter-publication-refresh";
    let units = [store::ProbeJobUnitSpec {
        ledger_id: &id,
        kind: "tv",
        force_fingerprint: true,
        reuse_fingerprint_cache: false,
        overwrite_markers: true,
        reuse_media_info_cache: true,
    }];
    assert!(
        f.store
            .create_probe_job(
                job,
                "marker_refresh",
                &f.file.media_id.to_string(),
                None,
                "test",
                &units
            )
            .unwrap()
    );
    assert!(f.store.start_probe_unit(job, &id).unwrap());
    f.store.finish_probe_unit(job, &id, true, None).unwrap();
    let replacement = MarkerResultReplacement {
        media_id: f.file.media_id,
        season: 1,
        markers: vec![f.marker(UnitKey::new(1, 1), 25_000)],
        targets: vec![f.row()],
        chapter_updates: vec![(id, vec![])],
    };
    f.store
        .complete_marker_refresh_checked(job, &[replacement], &f.members())
        .unwrap();
    assert_eq!(
        f.store.get_probe_job(job).unwrap().unwrap().status,
        "succeeded"
    );
    assert!(f.targets().contains(&(25_000, 35_000)));
}

#[test]
fn checked_cold_publication_keeps_physical_cache_separate_from_marker_overlays() {
    let f = Fixture::new(&[UnitKey::new(1, 1)]);
    let physical = vec![chapter(0, 10_000), chapter(20_000, 30_000)];
    let marker = f.marker(UnitKey::new(1, 1), 40_000);
    let row = f.row();
    let members = f.members();
    f.store
        .library_write(|db| {
            db.publish_file_chapters_checked(
                &row,
                marker.unit,
                &members,
                &[marker],
                Some(&physical),
            )
        })
        .unwrap();
    assert_eq!(
        f.store
            .get_cached_chapters(&f.file.file_id.to_string())
            .unwrap()
            .unwrap(),
        physical
    );
    assert_eq!(
        f.targets(),
        vec![(0, 10_000), (20_000, 30_000), (40_000, 50_000)]
    );
    let before = f
        .store
        .current_chapter_frame_task(f.file.file_id)
        .unwrap()
        .unwrap();
    let mut stale = f.members();
    stale[0].expected.source_version = "stale".into();
    assert!(
        f.store
            .library_write(|db| db.publish_file_chapters_checked(
                &row,
                UnitKey::new(1, 1),
                &stale,
                &[],
                Some(&[])
            ))
            .is_err()
    );
    assert_eq!(
        f.store
            .current_chapter_frame_task(f.file.file_id)
            .unwrap()
            .unwrap(),
        before
    );
    assert_eq!(
        f.store
            .get_cached_chapters(&f.file.file_id.to_string())
            .unwrap()
            .unwrap(),
        physical
    );
}

#[test]
fn derived_synthetic_cache_entries_are_not_reintroduced_as_physical_targets() {
    let f = Fixture::new(&[UnitKey::new(1, 1)]);
    let mut stale = chapter(999_000, 1_000_000);
    stale.synthetic = true;
    f.store
        .put_cached_chapters(
            &f.file.file_id.to_string(),
            &[chapter(0, 10_000), chapter(20_000, 30_000), stale],
        )
        .unwrap();
    assert_eq!(f.targets(), vec![(0, 10_000), (20_000, 30_000)]);
}

#[test]
fn canonical_coverage_publication_removes_the_retired_units_frame_targets() {
    let f = Fixture::new(&[UnitKey::new(1, 1), UnitKey::new(1, 2)]);
    for (unit, start) in [(UnitKey::new(1, 1), 10_000), (UnitKey::new(1, 2), 80_000)] {
        f.store
            .put_file_marker_checked(&f.marker(unit, start), &f.members())
            .unwrap();
    }
    let before = f
        .store
        .current_chapter_frame_task(f.file.file_id)
        .unwrap()
        .unwrap();
    assert!(before.task.targets.contains(&(80_000, 90_000)));
    let file = publish_fixture_file_units(
        &f.store,
        &FixtureFile {
            media_id: f.file.media_id,
            path: f.file.path.display().to_string(),
            ..Default::default()
        },
        &[UnitKey::new(1, 1)],
    )
    .unwrap();
    assert_eq!(file.version.source_version, f.file.version.source_version);
    assert!(
        f.store
            .get_file_marker(file.file_id, UnitKey::new(1, 2))
            .unwrap()
            .is_none()
    );
    let current = f
        .store
        .current_chapter_frame_task(file.file_id)
        .unwrap()
        .unwrap();
    assert!(
        !current.task.targets.contains(&(80_000, 90_000)),
        "retired UnitKey must leave the authoritative file target set"
    );
    assert_ne!(current.task.id, before.task.id);
    assert!(current.task.targets.contains(&(10_000, 20_000)));
}

#[test]
fn binding_proven_placed_bytes_rebinds_the_preserved_chapters_frame_obligation() {
    use store::library_write::{FilePublication, WriteScope};
    let f = Fixture::new(&[UnitKey::new(1, 1)]);
    let path = f.file.path.parent().unwrap().join("placed.mkv");
    let mut file = publish_fixture_file_units(
        &f.store,
        &FixtureFile {
            media_id: f.file.media_id,
            path: path.display().to_string(),
            ..Default::default()
        },
        &[UnitKey::new(1, 2)],
    )
    .unwrap();
    std::fs::write(&path, b"newly placed bytes").unwrap();
    let observation = library::observe_source(&path).unwrap();
    assert_ne!(observation.source_version, file.version.source_version);
    file.size_bytes = observation.size_bytes;
    file.modified_ns = observation.modified_ns;
    file.stat_known = true;
    let scope = WriteScope {
        library_id: file.library_id,
        config_revision: f
            .store
            .library_config_revision(&file.library_id.to_string())
            .unwrap(),
    };
    f.store
        .publish_files(
            &scope,
            &[FilePublication {
                file: &file,
                units: &[UnitKey::new(1, 2)],
                expected_version: Some(&file.version),
                nfo_observation: None,
                probe_required: false,
            }],
            1,
        )
        .unwrap();
    let chapters = vec![chapter(0, 10_000), chapter(10_000, 20_000)];
    f.store
        .put_cached_chapters(&file.file_id.to_string(), &chapters)
        .unwrap();
    let old = f
        .store
        .current_chapter_frame_task(file.file_id)
        .unwrap()
        .unwrap();
    let bound = f
        .store
        .bind_placed_file_source(&scope, file.file_id, &file.version, &observation, 2)
        .unwrap();
    assert_eq!(
        f.store
            .get_cached_chapters(&file.file_id.to_string())
            .unwrap(),
        Some(chapters)
    );
    let current = f
        .store
        .current_chapter_frame_task(file.file_id)
        .unwrap()
        .expect("rebound physical chapters need a current-source obligation");
    assert_eq!(current.task.source_version, bound.source_version);
    assert_eq!(current.task.targets, old.task.targets);
    assert_ne!(current.status.generation, old.status.generation);
    assert_eq!(
        f.store
            .chapter_frame_task_status(&old.task.id)
            .unwrap()
            .unwrap()
            .state,
        store::ChapterFrameState::Superseded
    );
}
