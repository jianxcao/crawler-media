//! R20/S02 measurement: the integrity checker must stay bounded per batch on a
//! 100k-Media Library. Ignored by default (seeding takes minutes); run with
//! `INTEGRITY_SCALE_N=100000 cargo test --release -p store --test library_integrity_scale -- --ignored --nocapture`.

use std::time::{Duration, Instant};

use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, LibraryId,
    Media, MediaId, MediaKind, QualitySource, UnitKey,
};
use store::Store;
use store::library_write::{FilePublication as StorePublication, WriteScope};

fn seed(store: &Store, n: usize) -> LibraryId {
    let lib = store
        .create_library(
            MediaKind::Movie,
            "Scale",
            &["/scale"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let library_id: LibraryId = lib.id.parse().unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|r| r.library_id.as_str() == lib.id.as_str())
        .unwrap()
        .id;
    let mut files = Vec::with_capacity(100);
    for n in 0..n {
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: format!("Movie {n}"),
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
        files.push(LibraryFile {
            file_id: LedgerId::new(),
            library_id,
            root_id: root_id.clone(),
            media_id: media.id,
            kind: MediaKind::Movie,
            path: format!("/scale/m{n}.mkv").into(),
            version: FileVersion {
                revision: 1,
                source_version: "v1".into(),
            },
            presence: FilePresence::Present,
            publication: FilePublication::Published,
            size_bytes: Some(1024),
            stat_known: true,
            created_ns: Some(0),
            modified_ns: Some(0),
            source_digest: None,
            source_path: None,
            transfer_mode: None,
            resolution: Some("1080p".into()),
            codec: None,
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
            release_quality: None,
            duration_ms: None,
            tracks_version: 1,
            poster_path: None,
            backdrop_path: None,
            still_path: None,
        });
        if files.len() == 100 {
            publish(store, library_id, &files);
            files.clear();
            if (n + 1) % 5000 == 0 {
                eprintln!("seed progress: {} Media", n + 1);
            }
        }
    }
    publish(store, library_id, &files);
    library_id
}

fn publish(store: &Store, library_id: LibraryId, files: &[LibraryFile]) {
    if files.is_empty() {
        return;
    }
    let publications: Vec<StorePublication<'_>> = files
        .iter()
        .map(|file| StorePublication {
            file,
            units: &[UnitKey::WHOLE],
            expected_version: None,
            nfo_observation: None,
            probe_required: false,
        })
        .collect();
    let t = Instant::now();
    store
        .publish_files(
            &WriteScope {
                library_id,
                config_revision: 0,
            },
            &publications,
            1000,
        )
        .unwrap();
    if std::env::var_os("INTEGRITY_SCALE_TRACE").is_some() {
        eprintln!("publish {} files: {:?}", files.len(), t.elapsed());
    }
}

#[test]
#[ignore = "scale measurement; see module docs"]
fn integrity_checker_batches_stay_bounded_at_scale() {
    let n: usize = std::env::var("INTEGRITY_SCALE_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000);
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let seeded = Instant::now();
    let library = seed(&store, n);
    eprintln!("seeded {n} Media in {:?}", seeded.elapsed());

    store
        .begin_library_integrity(library, false, "op-scale", 1)
        .unwrap();
    let started = Instant::now();
    let mut batches = 0usize;
    let mut slowest = Duration::ZERO;
    let mut first_ten = Duration::ZERO;
    let mut last_ten = std::collections::VecDeque::new();
    loop {
        let batch = Instant::now();
        let status = store
            .advance_library_operation("op-scale", 2 + batches as i64, 100)
            .unwrap();
        let took = batch.elapsed();
        batches += 1;
        slowest = slowest.max(took);
        if batches <= 10 {
            first_ten += took;
        }
        last_ten.push_back(took);
        if last_ten.len() > 10 {
            last_ten.pop_front();
        }
        if batches % 100 == 0 {
            eprintln!(
                "batch {batches}: {took:?} (elapsed {:?})",
                started.elapsed()
            );
        }
        if status.state != "applying" {
            assert_eq!(status.state, "succeeded");
            break;
        }
        assert!(batches <= n / 100 + 10, "batch count must track n/100");
    }
    let last: Duration = last_ten.iter().sum();
    eprintln!(
        "checked {n} Media: {batches} batches, total {:?}, slowest batch {slowest:?}, \
         first-10 {first_ten:?}, last-10 {last:?}",
        started.elapsed()
    );
    assert!(
        !store
            .library_integrity_report("op-scale")
            .unwrap()
            .has_issues()
    );
    // Bounded work per batch: the tail of the Library costs about what the head does
    // (a whole-Library read per batch would make late batches far slower or the
    // first one pay for everything).
    assert!(
        last <= first_ten * 4 + Duration::from_millis(200),
        "late batches must not grow with Library size: first {first_ten:?}, last {last:?}"
    );
}
