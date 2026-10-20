//! Batch read facts: `Store::media_batch` and
//! `Store::playback_units_for_user` (full `UnitState`, not a reduced row).

use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource, UserId};
use std::path::Path;
use store::Store;

fn media(kind: MediaKind, title: &str, tmdb_id: Option<&str>) -> Media {
    Media {
        id: MediaId::new(),
        kind,
        title: title.into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: tmdb_id.map(str::to_string),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

/// Publish one file covering `(season, episode)` for `media_id`. Resume
/// positions only bind to a published, present file unit, so the fixture must
/// exist before `upsert_unit` records a position.
fn publish_unit(store: &Store, base: &Path, media_id: MediaId, season: u32, episode: u32) {
    let root = ensure_library(store, base, media_id);
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: root
            .join(format!("{media_id}-s{season}e{episode}.mkv"))
            .display()
            .to_string(),
        season: Some(season),
        episode: Some(episode),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    store.insert_ledger(&row).unwrap();
}

/// A Library root is owned by exactly one Library and only owns files of its
/// own kind, so the fixture creates a kind-specific Library for `base`.
fn ensure_library(store: &Store, base: &Path, media_id: MediaId) -> std::path::PathBuf {
    let kind = store.get_media(media_id).unwrap().unwrap().kind;
    let root = base.join(kind.as_str());
    if store
        .list_library_roots()
        .unwrap()
        .iter()
        .any(|candidate| candidate.path == root && candidate.kind == kind)
    {
        return root;
    }
    store
        .create_library(
            kind,
            &format!("lib-{}-{}", kind.as_str(), base.display()),
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    root
}

/// Publish one whole-file (`-1/-1`) source for `media_id`.
fn publish_whole(store: &Store, base: &Path, media_id: MediaId) {
    let root = ensure_library(store, base, media_id);
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: root
            .join(format!("whole-{media_id}.mkv"))
            .display()
            .to_string(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    store.insert_ledger(&row).unwrap();
}

#[test]
fn media_batch_returns_requested_ids_and_skips_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let movie = media(MediaKind::Movie, "Shared alias", Some("603"));
    let show = media(MediaKind::Tv, "Shared alias", Some("603"));
    store.insert_media(&movie).unwrap();
    store.insert_media(&show).unwrap();
    let missing = MediaId::new();

    let found = store.media_batch(&[movie.id, show.id, missing]).unwrap();

    assert_eq!(found.len(), 2, "same numeric alias must not merge rows");
    assert_eq!(found.get(&movie.id).unwrap().kind, MediaKind::Movie);
    assert_eq!(found.get(&show.id).unwrap().kind, MediaKind::Tv);
    assert_eq!(found.get(&show.id).unwrap().tmdb_id.as_deref(), Some("603"));
    assert!(!found.contains_key(&missing));
}

#[test]
fn media_batch_empty_input_returns_empty_map() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    assert!(store.media_batch(&[]).unwrap().is_empty());
}

#[test]
fn media_batch_spans_the_500_id_chunk_boundary() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let mut ids = Vec::new();
    for index in 0..501 {
        let item = media(MediaKind::Movie, &format!("item {index}"), None);
        store.insert_media(&item).unwrap();
        ids.push(item.id);
    }

    let found = store.media_batch(&ids).unwrap();
    assert_eq!(found.len(), 501);
    for id in ids {
        assert!(found.contains_key(&id), "missing {id}");
    }
}

#[test]
fn playback_units_for_user_keeps_full_state_for_partial_progress() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let show = media(MediaKind::Tv, "Partial", Some("603"));
    store.insert_media(&show).unwrap();
    let root = tmp.path().join("media");
    publish_unit(&store, &root, show.id, 1, 2);
    let user = UserId::new();
    store
        .upsert_unit(
            user,
            show.id,
            1,
            2,
            123_456,
            Some(false),
            Some(true),
            Some(3_600_000),
            Some("jpn"),
            Some("zh"),
            false,
            42,
        )
        .unwrap();
    store
        .upsert_unit(user, show.id, -1, -1, 0, Some(true), Some(false), None, None, None, true, 43)
        .unwrap();

    let units = store.playback_units_for_user(user).unwrap();
    assert_eq!(units.len(), 2);
    let partial = units
        .iter()
        .find(|unit| unit.season == 1 && unit.episode == 2)
        .expect("partial unit");
    assert_eq!(partial.media_id, show.id);
    assert_eq!(partial.state.position_ms, 123_456);
    assert!(!partial.state.played, "watched mark must not be implied");
    assert!(partial.state.favorite);
    assert_eq!(partial.state.duration_ms, Some(3_600_000));
    assert_eq!(partial.state.audio_track.as_deref(), Some("jpn"));
    assert_eq!(partial.state.subtitle_track.as_deref(), Some("zh"));
    assert_eq!(partial.state.play_count, 0);
    assert_eq!(partial.state.updated_at, 42);
    assert_eq!(
        partial.state,
        store.unit_state(user, show.id, 1, 2).unwrap().unwrap()
    );
}

#[test]
fn favorite_only_update_preserves_played_activity_timestamp() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let user = UserId::new();
    let media = media(MediaKind::Tv, "Favorite timestamp", None);
    store.insert_media(&media).unwrap();
    store.upsert_unit(user, media.id, 1, -1, 0, Some(true), Some(false), None, None, None, false, 10).unwrap();
    store.set_unit_favorite(user, media.id, 1, -1, true, 99).unwrap();
    let state = store.unit_state(user, media.id, 1, -1).unwrap().unwrap();
    assert!(state.played);
    assert!(state.favorite);
    assert_eq!(state.updated_at, 10);
}

#[test]
fn whole_unit_unfavorite_preserves_other_units_activity_timestamps() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let user = UserId::new();
    let media = media(MediaKind::Tv, "Cascade favorite timestamp", None);
    store.insert_media(&media).unwrap();
    store.upsert_unit(user, media.id, -1, -1, 0, Some(false), Some(true), None, None, None, false, 5).unwrap();
    store.upsert_unit(user, media.id, 1, -1, 0, Some(false), Some(true), None, None, None, false, 10).unwrap();
    store.upsert_unit(user, media.id, 1, 1, 0, Some(false), Some(true), None, None, None, false, 15).unwrap();

    store.set_unit_marks(user, media.id, -1, -1, None, Some(false), 99).unwrap();

    assert_eq!(store.unit_state(user, media.id, -1, -1).unwrap().unwrap().updated_at, 5);
    assert_eq!(store.unit_state(user, media.id, 1, -1).unwrap().unwrap().updated_at, 10);
    assert_eq!(store.unit_state(user, media.id, 1, 1).unwrap().unwrap().updated_at, 15);
    assert!(!store.unit_state(user, media.id, 1, 1).unwrap().unwrap().favorite);
}

#[test]
fn favorite_activity_orders_independently_of_played_activity() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let user = UserId::new();
    let older = media(MediaKind::Movie, "Older favorite", None);
    let newer = media(MediaKind::Movie, "Newer favorite", None);
    store.insert_media(&older).unwrap();
    store.insert_media(&newer).unwrap();
    store
        .upsert_unit(user, older.id, -1, -1, 0, Some(true), Some(true), None, None, None, false, 10)
        .unwrap();
    store
        .upsert_unit(user, newer.id, -1, -1, 0, Some(true), Some(false), None, None, None, false, 50)
        .unwrap();

    store
        .set_unit_marks(user, newer.id, -1, -1, None, Some(true), 80)
        .unwrap();

    let rows = store.favorite_rows(user).unwrap();
    assert_eq!(rows[0].0, newer.id, "a new favourite must lead recent favourites");
    assert_eq!(rows[0].1, 80);
    let played = store.user_active_media_ids(user).unwrap();
    assert_eq!(played[0].0, newer.id);
    assert_eq!(played[0].1, 50, "favouriting must not refresh the played clock");
    assert_eq!(store.unit_state(user, newer.id, -1, -1).unwrap().unwrap().updated_at, 50);
}

#[test]
fn favorite_only_insert_does_not_materialize_inherited_played() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let user = UserId::new();
    let show = media(MediaKind::Tv, "Inherited played", None);
    store.insert_media(&show).unwrap();
    store
        .upsert_unit(user, show.id, 1, -1, 0, Some(true), None, None, None, None, false, 10)
        .unwrap();

    store
        .set_unit_favorite(user, show.id, 1, 1, true, 20)
        .unwrap();
    store.clear_units(user, None, Some(1)).unwrap();

    let episode = store.unit_state(user, show.id, 1, 1).unwrap().unwrap();
    assert!(episode.favorite);
    assert!(!episode.played, "inherited played must not survive as an explicit fact");
    assert_eq!(episode.updated_at, 0);
}

#[test]
fn playback_units_for_user_is_scoped_to_one_user() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let movie = media(MediaKind::Movie, "Scoped", None);
    store.insert_media(&movie).unwrap();
    publish_whole(&store, tmp.path(), movie.id);
    let owner = UserId::new();
    store
        .upsert_unit(owner, movie.id, -1, -1, 5_000, Some(false), Some(false), None, None, None, false, 7)
        .unwrap();

    assert_eq!(store.playback_units_for_user(owner).unwrap().len(), 1);
    assert!(store.playback_units_for_user(UserId::new()).unwrap().is_empty());
}

/// Design characterization only: no derived owner index exists yet, so this
/// checks current candidate/owner parity rather than claiming a regression.
#[test]
fn default_candidate_growth_and_ownership_parity() {
    use domain::{Confidence, LedgerId, LedgerRow, QualitySource};
    use std::path::Path;

    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("root");
    let nested = root.join("nested");
    let tied_root = tmp.path().join("tied");
    let target = store
        .create_library(MediaKind::Movie, "Target", &[root.to_str().unwrap()], "everyone", true, &[])
        .unwrap();
    let nested_library = store
        .create_library(MediaKind::Movie, "Nested", &[nested.to_str().unwrap()], "everyone", true, &[])
        .unwrap();
    let tied = store
        .create_library(MediaKind::Movie, "Tied", &[tied_root.to_str().unwrap()], "everyone", true, &[])
        .unwrap();
    let default = store.default_library(MediaKind::Movie).unwrap().unwrap();
    let paths = [
        root.join("owned.mkv").display().to_string(),
        nested.join("nested.mkv").display().to_string(),
        tied_root.join("ambiguous.mkv").display().to_string(),
        format!("{}/./odd.mkv", root.display()),
    ];
    for (index, path) in paths.iter().enumerate() {
        let item = media(MediaKind::Movie, &format!("parity item {index}"), None);
        store.insert_media(&item).unwrap();
        let row = LedgerRow {
            id: LedgerId::new(), media_id: item.id, path: path.clone(), season: None,
            episode: None, resolution: None, codec: None, hdr: None,
            quality_source: QualitySource::Release, confidence: Confidence::High, filter_score: None,
        };
        store.insert_ledger(&row).unwrap();
        if index == 0 {
            let tv_row = LedgerRow { id: LedgerId::new(), media_id: row.media_id, path: path.clone(), ..row.clone() };
            store.insert_ledger(&tv_row).unwrap();
        }
    }
    let ownership = store.library_ownership().unwrap();
    let frozen = store.begin_library_removal(&target.id).unwrap();
    assert_eq!(frozen.library_id, target.id);
    let ownership = store.library_ownership().unwrap();
    for library_id in [&target.id, &nested_library.id, &tied.id, &default.id] {
        let current = store.list_ledger().unwrap().into_iter().filter(|row| {
            ownership.owner(Path::new(&row.path), MediaKind::Movie).is_some_and(|owner| owner.id == *library_id)
        }).map(|row| row.id).collect::<std::collections::HashSet<_>>();
        let candidate = store.ledger_candidates_for_library(&ownership, library_id).unwrap().into_iter().filter(|row| {
            ownership.owner(Path::new(&row.path), MediaKind::Movie).is_some_and(|owner| owner.id == *library_id)
        }).map(|row| row.id).collect::<std::collections::HashSet<_>>();
        assert_eq!(candidate, current);
    }
    let cold_global_build_is_reported = store.list_ledger().unwrap().len() == 4;
    assert!(cold_global_build_is_reported, "the existing default candidate build reads globally");
    // No derived index/interface exists today; stale-index rejection is therefore not applicable.
    let db = rusqlite::Connection::open(tmp.path().join("data/library.db")).unwrap();
    let index_exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='browse_ledger_owners')", [], |r| r.get(0)).unwrap();
    assert!(!index_exists, "documented absence: derived ownership index is unimplemented");
}

/// Measurement only: current global-scan growth and persisted bytes/write time.
#[test]
#[ignore = "measurement; run with --ignored --nocapture"]
fn default_candidate_scan_size_and_write_cost() {
    use domain::{Confidence, LedgerId, LedgerRow, QualitySource};
    use std::time::Instant;

    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let default = store.default_library(MediaKind::Movie).unwrap().unwrap();
    let item = media(MediaKind::Movie, "unrelated", None);
    store.insert_media(&item).unwrap();
    let mut total_rows = 0usize;
    for count in [1_000usize, 10_000, 50_000] {
        let started = Instant::now();
        for i in 0..count {
            store.insert_ledger(&LedgerRow {
                id: LedgerId::new(), media_id: item.id, path: format!("/unrelated/{count}/{i}.mkv"),
                season: None, episode: None, resolution: None, codec: None, hdr: None,
                quality_source: QualitySource::Release, confidence: Confidence::High, filter_score: None,
            }).unwrap();
        }
        total_rows += count;
        let write_ms = started.elapsed().as_millis();
        let ownership = store.library_ownership().unwrap();
        let started = Instant::now();
        let rows = store.ledger_candidates_for_library(&ownership, &default.id).unwrap();
        let scan_ms = started.elapsed().as_millis();
        let bytes = std::fs::metadata(tmp.path().join("data/library.db")).unwrap().len();
        println!("batch_rows={count} cumulative_rows={total_rows} candidates={} scan_ms={scan_ms} batch_insert_ms={write_ms} library_db_bytes={bytes}", rows.len());
    }
}
