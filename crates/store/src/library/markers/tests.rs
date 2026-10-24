use super::{MarkerResultReplacement, Store, StoredMediaMarker};
use crate::library_write::publish::{insert_file_row, sync_coverage};
use crate::probe_tasks::ProbeJobUnitSpec;
use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, LibraryId,
    Media, MediaId, MediaKind, QualitySource, UnitKey,
};

fn marker(
    media_id: domain::MediaId,
    season: u32,
    source: &str,
    start_ms: i64,
) -> StoredMediaMarker {
    StoredMediaMarker {
        media_id,
        season,
        episode: 1,
        intro_start_ms: Some(start_ms),
        intro_end_ms: Some(start_ms + 60_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: source.to_string(),
        locked: false,
        updated_at: 1,
    }
}

/// One Media plus the Library/root that owns its files.
struct Fixture {
    library_id: LibraryId,
    root_id: String,
    root: std::path::PathBuf,
    media_id: MediaId,
    kind: MediaKind,
}

fn seed_fixture(store: &Store, dir: &std::path::Path, kind: MediaKind) -> Fixture {
    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
            kind,
            title: "Fixture".into(),
            year: Some(2024),
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
            aliases: vec![],
        })
        .unwrap();
    let root = dir.to_path_buf();
    let library = store
        .create_library(
            kind,
            "Fixture",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|entry| entry.library_id.as_str() == library.id.as_str())
        .map(|entry| entry.id)
        .unwrap();
    Fixture {
        library_id: library.id.parse().unwrap(),
        root_id,
        root,
        media_id,
        kind,
    }
}

/// Publish one real file fact row for `(season, episode)`, present on disk.
fn seed_file(store: &Store, fixture: &Fixture, name: &str, season: i32, episode: i32) -> LedgerId {
    let file_id = LedgerId::new();
    let path = fixture.root.join(name);
    std::fs::write(&path, b"fixture").unwrap();
    let file = LibraryFile {
        file_id,
        library_id: fixture.library_id,
        root_id: fixture.root_id.clone(),
        media_id: fixture.media_id,
        kind: fixture.kind,
        path,
        version: FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(7),
        stat_known: true,
        created_ns: Some(1),
        modified_ns: Some(1),
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: None,
        tracks_version: 0,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    };
    insert_file_row(&store.library, &file).unwrap();
    sync_coverage(&store.library, &file, &[UnitKey::new(season, episode)]).unwrap();
    file_id
}

/// Seed two seasons with old markers/chapters and finish both units of a refresh job.
fn two_season_refresh_setup(
    store: &Store,
    dir: &std::path::Path,
    job_id: &str,
) -> (MediaId, Vec<LedgerId>, Vec<library::ChapterMarker>) {
    let old_chapters = vec![library::ChapterMarker {
        start_ms: 0,
        end_ms: 300_000,
        title: Some("原章节".into()),
        marker_type: None,
        synthetic: false,
    }];
    let fixture = seed_fixture(store, dir, MediaKind::Tv);
    let file_ids = [1, 2]
        .map(|season| seed_file(store, &fixture, &format!("s{season}.mkv"), season, 1))
        .to_vec();
    for (season, file_id) in [1u32, 2].iter().zip(&file_ids) {
        store
            .put_media_marker(&marker(
                fixture.media_id,
                *season,
                "old",
                *season as i64 * 100_000,
            ))
            .unwrap();
        store
            .put_cached_chapters(&file_id.to_string(), &old_chapters)
            .unwrap();
    }
    let unit_ids: Vec<String> = file_ids.iter().map(|id| id.to_string()).collect();
    let specs: Vec<ProbeJobUnitSpec<'_>> = unit_ids
        .iter()
        .map(|unit_id| ProbeJobUnitSpec {
            ledger_id: unit_id.as_str(),
            kind: "tv",
            force_fingerprint: true,
            reuse_fingerprint_cache: false,
            overwrite_markers: true,
            reuse_media_info_cache: true,
        })
        .collect();
    assert!(
        store
            .create_probe_job(
                job_id,
                "marker_refresh",
                &fixture.media_id.to_string(),
                None,
                "marker-refresh-test",
                &specs,
            )
            .unwrap()
    );
    for unit_id in &unit_ids {
        assert!(store.start_probe_unit(job_id, unit_id).unwrap());
        store
            .finish_probe_unit(job_id, unit_id, true, None)
            .unwrap();
    }
    (fixture.media_id, file_ids, old_chapters)
}

#[test]
fn multi_season_refresh_keeps_old_results_on_failure_and_completes_atomically() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let job_id = "multi-season-refresh";
    let (media_id, file_ids, old_chapters) = two_season_refresh_setup(&store, temp.path(), job_id);

    let s1_new = marker(media_id, 1, "new", 111_000);
    let s2_duplicate = marker(media_id, 2, "new", 222_000);
    let replacements = [
        MarkerResultReplacement {
            media_id,
            season: 1,
            markers: vec![s1_new],
            chapter_updates: vec![(
                file_ids[0].to_string(),
                vec![library::ChapterMarker {
                    start_ms: 111_000,
                    end_ms: 171_000,
                    title: Some("新片头".into()),
                    marker_type: Some(library::MarkerType::IntroStart),
                    synthetic: false,
                }],
            )],
        },
        MarkerResultReplacement {
            media_id,
            season: 2,
            markers: vec![s2_duplicate.clone(), s2_duplicate],
            chapter_updates: vec![(file_ids[1].to_string(), Vec::new())],
        },
    ];

    assert!(
        store
            .complete_marker_refresh(job_id, &replacements)
            .is_err()
    );
    assert_eq!(
        store.get_probe_job(job_id).unwrap().unwrap().status,
        "running"
    );
    for season in [1, 2] {
        assert_eq!(
            store
                .get_media_marker(media_id, Some(season), Some(1))
                .unwrap()
                .unwrap()
                .source,
            "old"
        );
        assert_eq!(
            store
                .get_cached_chapters(&file_ids[season as usize - 1].to_string())
                .unwrap()
                .unwrap(),
            old_chapters
        );
    }

    let valid_replacements = [
        MarkerResultReplacement {
            media_id,
            season: 1,
            markers: vec![marker(media_id, 1, "new", 111_000)],
            chapter_updates: vec![(file_ids[0].to_string(), old_chapters.clone())],
        },
        MarkerResultReplacement {
            media_id,
            season: 2,
            markers: vec![marker(media_id, 2, "new", 222_000)],
            chapter_updates: vec![(file_ids[1].to_string(), old_chapters.clone())],
        },
    ];
    store
        .complete_marker_refresh(job_id, &valid_replacements)
        .unwrap();
    assert_eq!(
        store.get_probe_job(job_id).unwrap().unwrap().status,
        "succeeded"
    );
    for (season, expected_source) in [(1, "new"), (2, "new")] {
        assert_eq!(
            store
                .get_media_marker(media_id, Some(season), Some(1))
                .unwrap()
                .unwrap()
                .source,
            expected_source
        );
    }
}

#[test]
fn locked_marker_preserves_and_syncs_chapters_across_file_versions() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (media_id, file_ids) = seed_locked_versions(&store, dir.path());

    let intro_from_cache = |id: &str| -> Option<Vec<(i64, i64)>> {
        store.get_cached_chapters(id).unwrap().map(|c| {
            c.into_iter()
                .filter(|c| c.marker_type == Some(library::MarkerType::IntroStart))
                .map(|c| (c.start_ms, c.end_ms))
                .collect()
        })
    };

    // When detected replacement arrives with 40s..100s, locked marker is preserved
    // and all versions are synchronized to locked marker (10s..70s).
    let detected_marker = StoredMediaMarker {
        media_id,
        season: 1,
        episode: 1,
        intro_start_ms: Some(40_000),
        intro_end_ms: Some(100_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: "voiceprint".into(),
        locked: false,
        updated_at: 2,
    };
    let updates: Vec<_> = file_ids
        .iter()
        .map(|id| {
            (
                id.to_string(),
                library::build_complete_timeline_chapters(&[], Some((40_000, 100_000)), None, None),
            )
        })
        .collect();
    store
        .replace_marker_results(media_id, 1, &[detected_marker], &updates)
        .unwrap();

    let cur_marker = store
        .get_media_marker(media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(cur_marker.intro_start_ms, Some(10_000));
    assert_eq!(cur_marker.intro_end_ms, Some(70_000));
    for id in &file_ids {
        assert_eq!(
            intro_from_cache(&id.to_string()),
            Some(vec![(10_000, 70_000)])
        );
    }

    // When empty replacements arrive, locked marker and chapters still remain 10s..70s.
    let empty_updates: Vec<_> = file_ids
        .iter()
        .map(|id| (id.to_string(), Vec::new()))
        .collect();
    store
        .replace_marker_results(media_id, 1, &[], &empty_updates)
        .unwrap();

    let cur_marker = store
        .get_media_marker(media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(cur_marker.intro_start_ms, Some(10_000));
    assert_eq!(cur_marker.intro_end_ms, Some(70_000));
    for id in &file_ids {
        assert_eq!(
            intro_from_cache(&id.to_string()),
            Some(vec![(10_000, 70_000)])
        );
    }
}

#[test]
fn locked_marker_rebuild_keeps_feature_within_known_media_duration() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let fixture = seed_fixture(&store, dir.path(), MediaKind::Tv);
    let media_id = fixture.media_id;
    let file_id = seed_file(&store, &fixture, "feature.mkv", 1, 1);
    let ledger_id = file_id.to_string();

    store
        .put_media_marker(&StoredMediaMarker {
            media_id,
            season: 1,
            episode: 1,
            intro_start_ms: Some(10_000),
            intro_end_ms: Some(70_000),
            outro_start_ms: None,
            outro_end_ms: None,
            source: "fixture".into(),
            locked: true,
            updated_at: 1,
        })
        .unwrap();
    store
        .put_file_meta_versioned(
            &ledger_id,
            &library::Tracks {
                video: None,
                audio: Vec::new(),
                subtitles: Vec::new(),
            },
            None,
            Some(1_000_000),
        )
        .unwrap();
    store
        .put_cached_chapters(
            &ledger_id,
            &library::build_complete_timeline_chapters(
                &[],
                Some((10_000, 70_000)),
                None,
                Some(1_000_000),
            ),
        )
        .unwrap();

    let feature_end = |store: &Store| -> Option<i64> {
        store
            .get_cached_chapters(&ledger_id)
            .unwrap()
            .and_then(|chapters| {
                chapters
                    .into_iter()
                    .find(|chapter| chapter.title.as_deref() == Some("正片"))
                    .map(|chapter| chapter.end_ms)
            })
    };
    assert_eq!(feature_end(&store), Some(1_000_000));

    store
        .replace_marker_results(media_id, 1, &[], &[(ledger_id.clone(), Vec::new())])
        .unwrap();

    assert_eq!(
        feature_end(&store),
        Some(1_000_000),
        "locked rebuild must keep the feature chapter inside the known media duration"
    );
}

#[test]
fn batch_replacement_stores_updated_at_in_unix_seconds() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let media_id = domain::MediaId::new();

    let mut detected = marker(media_id, 1, "voiceprint", 10_000);
    detected.updated_at = 0;
    let before_write = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    store
        .replace_marker_results(media_id, 1, &[detected], &[])
        .unwrap();
    let after_write = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let stored = store
        .get_media_marker(media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert!(
        (before_write..=after_write).contains(&stored.updated_at),
        "batch replacement must write the current Unix timestamp in seconds"
    );
}

fn seed_locked_versions(store: &Store, root: &std::path::Path) -> (MediaId, Vec<LedgerId>) {
    let fixture = seed_fixture(store, root, MediaKind::Tv);
    let file_ids = (0..3)
        .map(|v| seed_file(store, &fixture, &format!("version-{v}.mkv"), 1, 1))
        .collect::<Vec<_>>();
    let locked_marker = StoredMediaMarker {
        media_id: fixture.media_id,
        season: 1,
        episode: 1,
        intro_start_ms: Some(10_000),
        intro_end_ms: Some(70_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: "fixture".into(),
        locked: true,
        updated_at: 1,
    };
    store.put_media_marker(&locked_marker).unwrap();
    store
        .put_cached_chapters(
            &file_ids[0].to_string(),
            &library::build_complete_timeline_chapters(&[], Some((10_000, 70_000)), None, None),
        )
        .unwrap();
    store
        .put_cached_chapters(
            &file_ids[1].to_string(),
            &library::build_complete_timeline_chapters(&[], Some((20_000, 80_000)), None, None),
        )
        .unwrap();
    store
        .put_cached_chapters(&file_ids[2].to_string(), &[])
        .unwrap();

    (fixture.media_id, file_ids)
}
