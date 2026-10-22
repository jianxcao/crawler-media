use domain::{Confidence, LedgerId, MediaId, QualitySource};
use store::Store;

/// Fixture scan row: a whole-unit file with an 1080p release observation and no
/// NFO facts. The read shape carries the file identity directly.
fn sample_row(media_id: MediaId, path: &str) -> store::ScannedLedger {
    store::ScannedLedger {
        file_id: LedgerId::new(),
        media_id,
        path: path.to_string(),
        units: vec![domain::UnitKey::WHOLE],
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
        source_version: "fixture-v1".into(),
        source_digest: None,
        size_bytes: None,
        created_ns: None,
        modified_ns: None,
        observations: Vec::new(),
    }
}

fn setup(data_dir: &std::path::Path) -> (Store, MediaId) {
    let store = Store::open(data_dir).unwrap();
    store
        .create_library(
            domain::MediaKind::Movie,
            "Movies",
            &["/movies"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = domain::Media {
        id: MediaId::new(),
        kind: domain::MediaKind::Movie,
        title: "Test Movie".to_string(),
        year: Some(2024),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
        aliases: vec![],
    };
    store.insert_media(&media).unwrap();
    (store, media.id)
}
#[test]
fn probe_obligations_bounded_vm_steps_at_scale() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, media_id) = setup(&tmp.path().join("data"));

    // 100 fixed target obligations with low timestamps (1..=100)
    for index in 1..=100 {
        let row = sample_row(media_id, &format!("/movies/target_{index}.mkv"));
        store.insert_scanned_files(&[row], index).unwrap();
    }

    // 1k series baseline: 1,000 backlog items with higher timestamps
    for index in 101..=1_100 {
        let row = sample_row(media_id, &format!("/movies/backlog_{index}.mkv"));
        store
            .insert_scanned_files(&[row], 10_000 + index)
            .unwrap();
    }

    let (obs_1k, vm_steps_1k) = store.scan_probe_obligations_with_vm_steps(100).unwrap();
    assert_eq!(obs_1k.len(), 100);
    assert_eq!(obs_1k[0].created_at_ms, 1);
    assert!(vm_steps_1k > 0, "vm_steps_1k must be > 0");

    // Grow backlog to 50,000 items (49,000 additional rows)
    for index in 1_101..=50_100 {
        let row = sample_row(media_id, &format!("/movies/backlog_{index}.mkv"));
        store
            .insert_scanned_files(&[row], 10_000 + index)
            .unwrap();
    }

    let (obs_50k, vm_steps_50k) = store.scan_probe_obligations_with_vm_steps(100).unwrap();
    assert_eq!(obs_50k.len(), 100);
    assert_eq!(obs_50k, obs_1k);
    assert_eq!(obs_50k[0].created_at_ms, 1);
    assert!(
        vm_steps_50k <= vm_steps_1k * 2,
        "vm_steps_50k ({vm_steps_50k}) must be <= vm_steps_1k * 2 ({})",
        vm_steps_1k * 2
    );
}

#[test]
fn probe_obligations_limit_clamping() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, media_id) = setup(&tmp.path().join("data"));

    for index in 1..=150 {
        let row = sample_row(media_id, &format!("/movies/clamp_{index}.mkv"));
        store.insert_scanned_files(&[row], index).unwrap();
    }

    // limit 0 clamped to 1
    let clamped_zero = store.scan_probe_obligations(0).unwrap();
    assert_eq!(clamped_zero.len(), 1);
    assert_eq!(clamped_zero[0].created_at_ms, 1);

    // limit 101 clamped to 100
    let clamped_101 = store.scan_probe_obligations(101).unwrap();
    assert_eq!(clamped_101.len(), 100);
    assert_eq!(clamped_101[0].created_at_ms, 1);
}
