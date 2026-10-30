//! A11: source observation happens outside the store lock and publication only
//! compares the owned observation against ledger state inside the transaction.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use domain::{Confidence, FilePresence, LedgerId, MediaId, QualitySource};
use store::test_support::FixtureFile;
use store::{MarkerResultReplacement, SourceObservation, Store, StoredMediaMarker};

fn fixture(store: &Store, root: &std::path::Path, resolution: Option<&str>) -> FixtureFile {
    let media_id = MediaId::new();
    store
        .insert_media(&domain::Media {
            id: media_id,
            kind: domain::MediaKind::Movie,
            title: "Fixture".into(),
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
    store
        .create_library(
            domain::MediaKind::Movie,
            "lib",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let row = FixtureFile {
        file_id: Some(LedgerId::new()),
        media_id,
        path: root.join("movie.mkv").to_string_lossy().into_owned(),
        season: Some(1),
        episode: Some(1),
        resolution: resolution.map(Into::into),
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        ..Default::default()
    };
    std::fs::write(&row.path, b"fixture").unwrap();
    store::test_support::publish_fixture_file(store, &row).unwrap();
    row
}

fn marker(row: &FixtureFile, source: &str) -> StoredMediaMarker {
    StoredMediaMarker {
        media_id: row.media_id,
        season: 1,
        episode: 1,
        intro_start_ms: Some(10_000),
        intro_end_ms: Some(70_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: source.into(),
        locked: false,
        updated_at: 0,
    }
}

fn replacement(row: &FixtureFile) -> MarkerResultReplacement {
    MarkerResultReplacement {
        media_id: row.media_id,
        season: 1,
        markers: vec![marker(row, "new")],
        chapter_updates: vec![(row.file_id.unwrap().to_string(), vec![])],
    }
}

fn current_marker(store: &Store, row: &FixtureFile) -> Option<StoredMediaMarker> {
    store
        .get_media_marker(row.media_id, Some(1), Some(1))
        .unwrap()
}

/// Releases the gate even when an assertion unwinds, so the observer thread exits.
struct ReleaseOnDrop(Option<mpsc::Sender<()>>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}

#[test]
fn slow_source_observation_does_not_block_store() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(temp.path()).unwrap());
    let row = fixture(store.as_ref(), temp.path(), None);
    store
        .as_ref()
        .put_media_marker(&marker(&row, "old"))
        .unwrap();
    let before = current_marker(store.as_ref(), &row);
    let id = row.file_id.unwrap();

    // Owned snapshot under a short read; the lock is released before observing.
    let snapshots = store.as_ref().source_snapshots(&[id]).unwrap();
    let (release, gate) = mpsc::channel::<()>();
    let mut release = ReleaseOnDrop(Some(release));
    let (entered_tx, entered) = mpsc::channel::<()>();
    let observer = std::thread::spawn(move || {
        store::observe_sources_with(&snapshots, |path| {
            entered_tx.send(()).unwrap();
            gate.recv_timeout(Duration::from_secs(10)).unwrap();
            if path.exists() {
                FilePresence::Present
            } else {
                FilePresence::Missing
            }
        })
    });
    entered.recv_timeout(Duration::from_secs(5)).unwrap();

    // While observation is stuck, an independent store read completes promptly.
    let started = Instant::now();
    let reader = {
        let store = store.clone();
        std::thread::spawn(move || store.as_ref().source_snapshots(&[id]).unwrap().len())
    };
    let (done_tx, done) = mpsc::channel();
    std::thread::spawn(move || done_tx.send(reader.join().unwrap()).ok());
    let independent_read_finished_before_deadline = done
        .recv_timeout(Duration::from_secs(2))
        .is_ok_and(|n| n == 1)
        && started.elapsed() < Duration::from_secs(2);
    assert!(independent_read_finished_before_deadline);

    let _ = release.0.take().map(|tx| tx.send(()));
    let observed = observer.join().unwrap();
    assert_eq!(observed[0].presence, FilePresence::Present);

    // The file version moves after observation: publication is rejected, markers kept.
    std::fs::write(&row.path, b"rewritten after observation").unwrap();
    {
        let store = store.as_ref();
        let file = store
            .file_coverage_by_path(&row.path)
            .unwrap()
            .unwrap()
            .file;
        let scope = store::library_write::WriteScope {
            library_id: file.library_id,
            config_revision: store
                .library_config_revision(&file.library_id.to_string())
                .unwrap(),
        };
        let stat = api::fingerprint_job::observe_source(std::path::Path::new(&row.path)).unwrap();
        store
            .replace_file_source(&scope, id, &file.version, &stat, 5_000)
            .unwrap();
    }
    let moved = store.as_ref().source_snapshots(&[id]).unwrap();
    assert_ne!(
        moved[0].version, observed[0].expected,
        "fixture must move the version"
    );
    let markers_before = before;
    let stale_publication = store
        .as_ref()
        .replace_marker_results_batch_checked(&[replacement(&row)], &observed);
    assert!(stale_publication.is_err(), "{stale_publication:?}");
    let markers_after_rejection = current_marker(store.as_ref(), &row);
    assert_eq!(markers_after_rejection, markers_before);

    // Unknown observation keeps the last valid facts.
    let fresh = store.as_ref().source_snapshots(&[id]).unwrap();
    let unknown = store::observe_sources_with(&fresh, |_| FilePresence::Unknown);
    let unknown_observation = &unknown[0];
    assert_eq!(unknown_observation.presence, FilePresence::Unknown);
    assert!(
        store
            .as_ref()
            .replace_marker_results_batch_checked(&[replacement(&row)], &unknown)
            .is_err()
    );
    assert_eq!(current_marker(store.as_ref(), &row), markers_before);

    // Missing follows the explicit deletion policy (file_deleted), Present publishes.
    let missing: Vec<SourceObservation> =
        store::observe_sources_with(&fresh, |_| FilePresence::Missing);
    let error = store
        .as_ref()
        .replace_marker_results_batch_checked(&[replacement(&row)], &missing)
        .unwrap_err();
    assert!(error.to_string().contains("file_deleted"), "{error}");
    let present = store::observe_sources(&fresh);
    store
        .as_ref()
        .replace_marker_results_batch_checked(&[replacement(&row)], &present)
        .unwrap();
    assert_eq!(current_marker(store.as_ref(), &row).unwrap().source, "new");
}
