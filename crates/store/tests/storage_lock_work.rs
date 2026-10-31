//! Work gates for the 2026-10-28 storage lock review (L2/L3).
//!
//! Both gates compare the same public operation at two sizes. A quadratic or cubic
//! implementation fails the ratio; the linear one passes with room for timer noise.

use std::time::Instant;

use domain::{
    Confidence, FileVersion, LedgerId, Media, MediaId, MediaKind, ProtocolItemQuery,
    ProtocolItemType, ProtocolParent, QualitySource, UnitKey, UserId,
};
use store::library_write::probe::{Observed, ProbePublication};
use store::{ScannedLedger, Store};

fn tv_media(store: &Store) -> MediaId {
    let id = MediaId::new();
    store
        .insert_media(&Media {
            id,
            kind: MediaKind::Tv,
            title: "Work".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    id
}

fn ledger(media: MediaId, path: String, units: Vec<UnitKey>) -> ScannedLedger {
    ScannedLedger {
        file_id: LedgerId::new(),
        media_id: media,
        path,
        units,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
        source_version: "v1".into(),
        source_digest: None,
        size_bytes: None,
        created_ns: None,
        modified_ns: None,
        artwork: Default::default(),
        observations: vec![],
    }
}

/// Cumulative ms for probing every file of an `n`-episode TV Media once.
fn probe_all(n: i32) -> f64 {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let media = tv_media(&store);
    let lib = store
        .create_library(MediaKind::Tv, "Work", &["/work"], "everyone", true, &[])
        .unwrap();
    let rows: Vec<_> = (0..n)
        .map(|i| {
            ledger(
                media,
                format!("/work/e{i:05}.strm"),
                vec![UnitKey::new(1, i)],
            )
        })
        .collect();
    for batch in rows.chunks(100) {
        store.insert_scanned_files(batch, 1000).unwrap();
    }
    let expected = FileVersion {
        revision: 1,
        source_version: "v1".into(),
    };
    let publication = ProbePublication {
        expected: &expected,
        tracks_version: 1,
        resolution: Some("2160p"),
        codec: Some("h264"),
        hdr: Observed::Value(Some("HDR10")),
        duration_ms: Some(120_000),
        details: None,
    };
    let start = Instant::now();
    for row in &rows {
        assert!(
            store
                .publish_probe_result(row.file_id, &publication, 1001)
                .unwrap()
        );
    }
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
    let item = store
        .get_library_item(lib.id.parse().unwrap(), media)
        .unwrap()
        .unwrap();
    assert_eq!(item.distinct_episode_count, n as usize);
    elapsed
}

#[test]
fn per_file_probe_publication_does_not_rereduce_the_whole_media() {
    let small = probe_all(100);
    let large = probe_all(400);
    let ratio = large / small;
    eprintln!("probe_all: n=100 {small:.1}ms n=400 {large:.1}ms ratio={ratio:.2}");
    // Linear is ~4x; the full per-probe reduction measured ~57x.
    assert!(
        ratio < 10.0,
        "4x files cost {ratio:.1}x ({small:.1}ms -> {large:.1}ms)"
    );
}

fn played_series_vm_steps(episodes: i32) -> u64 {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let root = dir.path().join("tv");
    std::fs::create_dir_all(&root).unwrap();
    let lib = store
        .create_library(
            MediaKind::Tv,
            "TV",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = tv_media(&store);
    let user = UserId::new();
    store
        .insert_user(&domain::User {
            id: user,
            login: "work".into(),
            enabled: true,
            role: domain::UserRole::Member,
        })
        .unwrap();
    let units = (1..=episodes).map(|e| UnitKey::new(1, e)).collect();
    store
        .insert_scanned_files(
            &[ledger(
                media,
                root.join("pack.mkv").display().to_string(),
                units,
            )],
            1000,
        )
        .unwrap();
    let mut db = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
    let tx = db.transaction().unwrap();
    // Seed historical assertions in bulk; the final public mutation must also
    // publish the user projection consumed by the protocol query.
    for e in 1..episodes {
        tx.execute(
            "INSERT INTO playback_units(user_id,media_id,season,episode,played,has_played_assertion,played_changed_at_ms) VALUES(?1,?2,1,?3,1,1,2)",
            rusqlite::params![user.to_string(), media.to_string(), e],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    store
        .mutate_playback(
            media,
            &store::playback::PlaybackMutation::marks(user, 1, episodes, Some(true), None),
            2,
        )
        .unwrap();
    let summary = store
        .get_user_library_summary(user, lib.id.parse().unwrap(), media)
        .unwrap()
        .unwrap();
    assert_eq!(summary.completed_episode_count, i64::from(episodes));
    assert!(summary.played);
    let query = ProtocolItemQuery {
        user_id: user,
        parent: ProtocolParent::Library(lib.id.parse().unwrap()),
        series_id: None,
        season: None,
        person_ids: vec![],
        studio_ids: vec![],
        search_term: None,
        genres: vec![],
        genre_ids: vec![],
        filters: vec![],
        is_favorite: None,
        is_played: Some(true),
        include_types: vec![ProtocolItemType::Series],
        recursive: true,
        is_folder: None,
        sort: vec![],
        offset: 0,
        limit: 10,
    };
    let (page, stats) = store.protocol_item_page_with_stats(&query).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items.len(), 1);
    stats.vm_steps
}

#[test]
fn played_series_filter_is_linear_in_owned_episodes() {
    let small = played_series_vm_steps(250);
    let large = played_series_vm_steps(1000);
    let ratio = large as f64 / small as f64;
    eprintln!("played filter vm: e=250 {small} e=1000 {large} ratio={ratio:.2}");
    // Linear is ~4x; the per-unit scan of all assertions measured ~16x.
    assert!(
        ratio < 6.0,
        "4x episodes cost {ratio:.1}x VM steps ({small} -> {large})"
    );
}
