//! Work gates for the 2026-10-28 storage lock review (L2/L3).
//!
//! Both gates compare the same public operation at two sizes. A quadratic or cubic
//! implementation fails the ratio; the linear one passes with room for timer noise.

use domain::{
    Confidence, LedgerId, Media, MediaId, MediaKind, ProtocolItemQuery, ProtocolItemType,
    ProtocolParent, QualitySource, UnitKey, UserId,
};
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
    for e in 1..=episodes {
        tx.execute(
            "INSERT INTO playback_units(user_id,media_id,season,episode,played,has_played_assertion,played_changed_at_ms) VALUES(?1,?2,1,?3,1,1,2)",
            rusqlite::params![user.to_string(), media.to_string(), e],
        )
        .unwrap();
    }
    tx.commit().unwrap();
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
    stats.vm_steps as u64
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
