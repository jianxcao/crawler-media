//! An idempotent republish must cost nothing.
//!
//! Rescanning an unchanged Library republishes every file with the facts it already has.
//! That is the common case, so it must not write rows, reduce cards or rewrite coverage —
//! and the assertion has to be deterministic (SQLite's own write counter), because "it got
//! faster" is not a contract.
//!
//! Before Task 20 the same fixture wrote 7,000 rows per 1,000 unchanged files and reduced
//! 1,000 Media cards (`unchanged = 1000` in the report, all of that work wasted): the publish
//! loop wrote the row unconditionally, rewrote `library_file_units`, and pushed a
//! `PendingChange` for a file whose before/after facts were identical, so every Media got a
//! delta reduction. Now the row write is planned first, coverage is compared, and an
//! unchanged file contributes nothing to the reduction.

use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, LibraryId,
    Media, MediaId, MediaKind, QualitySource, UnitKey,
};
use store::Store;
use store::library_write::{
    FilePublication as StorePublication, PublishReport, PublishStats, WriteScope,
};

const SLOTS: usize = 1_000;
/// The publication batch bound; 1,000 files cost 10 transactions, as production does.
const BATCH: usize = 100;

struct Fixture {
    _temp: tempfile::TempDir,
    store: Store,
    scope: WriteScope,
    rows: Vec<LibraryFile>,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("data")).unwrap();
        let root = temp.path().join("movies");
        std::fs::create_dir_all(&root).unwrap();
        let library = store
            .create_library(
                MediaKind::Movie,
                "Movies",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let library_id: LibraryId = library.id.parse().unwrap();
        let root_id = store
            .list_library_roots()
            .unwrap()
            .into_iter()
            .find(|row| row.library_id == library.id)
            .unwrap()
            .id;
        let config_revision = store.library_config_revision(&library.id).unwrap();
        let rows = (0..SLOTS)
            .map(|index| {
                let media_id = MediaId::new();
                store
                    .insert_media(&Media {
                        id: media_id,
                        kind: MediaKind::Movie,
                        title: format!("Slot {index:05}"),
                        year: Some(2020),
                        original_title: None,
                        aliases: Vec::new(),
                        tmdb_id: None,
                        douban_id: None,
                        tvdb_id: None,
                        bangumi_id: None,
                        anilist_id: None,
                    })
                    .unwrap();
                file_row(
                    index,
                    library_id,
                    &root_id,
                    media_id,
                    root.to_str().unwrap(),
                )
            })
            .collect();
        Self {
            _temp: temp,
            store,
            scope: WriteScope {
                library_id,
                config_revision,
            },
            rows,
        }
    }

    /// Publish every row in production-sized batches; returns what one full pass cost.
    fn publish_all(&self) -> (PublishReport, PublishStats) {
        let mut report = PublishReport::default();
        let mut stats = PublishStats::default();
        for chunk in self.rows.chunks(BATCH) {
            let publications: Vec<StorePublication<'_>> = chunk
                .iter()
                .map(|file| StorePublication {
                    file,
                    units: &[UnitKey::WHOLE],
                    expected_version: None,
                    nfo_observation: None,
                    probe_required: false,
                })
                .collect();
            let (pass, pass_stats) = self
                .store
                .publish_files_with_stats(&self.scope, &publications, 1_000)
                .unwrap();
            report.inserted += pass.inserted;
            report.updated += pass.updated;
            report.unchanged += pass.unchanged;
            stats.written_rows += pass_stats.written_rows;
            stats.changed_files += pass_stats.changed_files;
            stats.unchanged_files += pass_stats.unchanged_files;
            stats.reduced_media += pass_stats.reduced_media;
            stats.coverage_syncs += pass_stats.coverage_syncs;
        }
        (report, stats)
    }

    fn item(&self, index: usize) -> Option<store::files::LibraryItemSummary> {
        let file = &self.rows[index];
        self.store
            .get_library_item(self.scope.library_id, file.media_id)
            .unwrap()
    }
}

fn file_row(
    index: usize,
    library_id: LibraryId,
    root_id: &str,
    media_id: MediaId,
    root: &str,
) -> LibraryFile {
    LibraryFile {
        file_id: LedgerId::new(),
        library_id,
        root_id: root_id.to_string(),
        media_id,
        kind: MediaKind::Movie,
        path: format!("{root}/slot-{index}.mkv").into(),
        version: FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(1_000_000_000 + (index as u64 % 40) * 250_000_000),
        stat_known: true,
        created_ns: Some(1_600_000_000_000_000_000 + index as i64 * 1_000_000),
        modified_ns: Some(1_600_000_000_000_000_000 + index as i64 * 1_000_000),
        source_digest: Some(format!("digest-{index}")),
        source_path: None,
        transfer_mode: None,
        resolution: Some(["1080p", "2160p", "720p"][index % 3].to_string()),
        codec: Some("h264".into()),
        hdr: Some(
            if index.is_multiple_of(4) {
                "HDR10"
            } else {
                "SDR"
            }
            .to_string(),
        ),
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: Some(100),
        release_quality: Some("RELEASE".into()),
        duration_ms: Some(7_200_000),
        tracks_version: 1,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    }
}

/// The whole point: an unchanged republish writes nothing and reduces nothing.
#[test]
fn unchanged_republish_writes_nothing() {
    let fix = Fixture::new();
    let (insert_report, insert) = fix.publish_all();
    assert_eq!(insert_report.inserted, SLOTS);
    assert!(insert.written_rows > 0, "the insert pass writes rows");
    assert_eq!(
        insert.reduced_media, SLOTS as u64,
        "each new Media is reduced once"
    );
    assert_eq!(
        insert.coverage_syncs, SLOTS as u64,
        "each new file records coverage"
    );

    let before = (0..SLOTS).map(|index| fix.item(index)).collect::<Vec<_>>();
    assert!(before.iter().all(Option::is_some), "every slot has a card");

    let (report, stats) = fix.publish_all();
    assert_eq!(
        report.unchanged, SLOTS,
        "the report already knew: {report:?}"
    );
    assert_eq!(report.inserted, 0);
    assert_eq!(report.updated, 0);
    assert_eq!(
        stats.written_rows, 0,
        "an unchanged republish must not write a single row: {stats:?}"
    );
    assert_eq!(
        stats.reduced_media, 0,
        "no card can change, so no card may be reduced: {stats:?}"
    );
    assert_eq!(
        stats.coverage_syncs, 0,
        "identical unit coverage must not be rewritten: {stats:?}"
    );
    assert_eq!(stats.changed_files, 0);
    assert_eq!(stats.unchanged_files, SLOTS as u64);

    // Worse than the latency: a republish must never move a published fact.
    for index in (0..SLOTS).step_by(97) {
        assert_eq!(
            fix.item(index),
            before[index],
            "slot {index} card changed on an idempotent republish"
        );
    }

    // Still idempotent the third time, and still free.
    let (third_report, third) = fix.publish_all();
    assert_eq!(third_report.unchanged, SLOTS);
    assert_eq!(third.written_rows, 0, "{third:?}");
}

/// The short-circuit must not swallow real work: a changed file still republishes.
#[test]
fn changed_file_still_republishes() {
    let mut fix = Fixture::new();
    let (_, _) = fix.publish_all();
    let card_before = fix.item(3).expect("card");

    // A new source version is a content change: revision bumps, facts move, card reduces.
    fix.rows[3].version = FileVersion {
        revision: 2,
        source_version: "v2".into(),
    };
    fix.rows[3].size_bytes = Some(2_000_000_000);

    let (report, stats) = fix.publish_all();
    assert_eq!(report.updated, 1, "one file changed: {report:?}");
    assert_eq!(report.unchanged, SLOTS - 1);
    assert_eq!(stats.changed_files, 1);
    assert_eq!(stats.reduced_media, 1, "only the changed Media is reduced");
    assert!(stats.written_rows > 0);

    let card_after = fix.item(3).expect("card");
    assert_ne!(
        card_before.size_bytes, card_after.size_bytes,
        "the changed file must move the card's size fact"
    );

    // ... and the pass after that is free again.
    let (_, idle) = fix.publish_all();
    assert_eq!(idle.written_rows, 0, "{idle:?}");
}

/// Coverage is part of the fact: a republish that changes only the unit split still writes.
#[test]
fn changed_coverage_still_republishes() {
    let fix = Fixture::new();
    let (_, _) = fix.publish_all();

    let mut publications = Vec::new();
    let two_units = [UnitKey::new(0, 1), UnitKey::new(0, 2)];
    publications.push(StorePublication {
        file: &fix.rows[0],
        units: &two_units,
        expected_version: None,
        nfo_observation: None,
        probe_required: false,
    });
    let (report, stats) = fix
        .store
        .publish_files_with_stats(&fix.scope, &publications, 1_000)
        .unwrap();
    assert_eq!(report.unchanged, 1, "the file row itself is current");
    assert_eq!(stats.coverage_syncs, 1, "the unit split changed");
    assert_eq!(stats.changed_files, 1);
    assert_eq!(stats.reduced_media, 1);
    assert!(stats.written_rows > 0, "{stats:?}");
}
