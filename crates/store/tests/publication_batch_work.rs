//! Q2: a publication batch finalizes each user summary once per Media, not once
//! per file. Measured as the extra time user facts add to a 100-file batch.

use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, Media, MediaId,
    MediaKind, QualitySource, UnitKey, UserId,
};
use store::Store;
use store::library_write::{FilePublication as StorePublication, WriteScope};
use store::playback::PlaybackMutation;

fn file(lib: &store::Library, root: &str, media: MediaId, ep: u32) -> LibraryFile {
    LibraryFile {
        file_id: LedgerId::new(),
        library_id: lib.id.parse().unwrap(),
        root_id: root.into(),
        media_id: media,
        kind: MediaKind::Tv,
        path: format!("Show/S01E{ep:03}.mkv").into(),
        version: FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(1_000),
        stat_known: true,
        created_ns: Some(100),
        modified_ns: Some(200),
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
        duration_ms: Some(1_000),
        tracks_version: 1,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    }
}

fn pubs(files: &[(LibraryFile, [UnitKey; 1])]) -> Vec<StorePublication<'_>> {
    files
        .iter()
        .map(|(f, u)| StorePublication {
            file: f,
            units: u,
            expected_version: None,
            nfo_observation: None,
            probe_required: false,
        })
        .collect()
}

/// Returns (summary writes during the batch, owned_episode_count of the first user).
fn batch(users: usize) -> (u64, i64) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Show".into(),
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
    let lib = store
        .create_library(MediaKind::Tv, "TV", &["/tv"], "everyone", true, &[])
        .unwrap();
    let root = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|r| r.library_id == Some(lib.id.clone()))
        .unwrap()
        .id;
    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };
    let first = [(file(&lib, &root, media.id, 0), [UnitKey::new(1, 0)])];
    store.publish_files(&scope, &pubs(&first), 1_000).unwrap();
    let ids: Vec<UserId> = (0..users).map(|_| UserId::new()).collect();
    for user in &ids {
        store
            .mutate_playback(
                media.id,
                &PlaybackMutation::marks(*user, 1, 0, Some(true), None),
                2_000,
            )
            .unwrap();
    }
    let files: Vec<_> = (1..=100)
        .map(|ep| {
            (
                file(&lib, &root, media.id, ep),
                [UnitKey::new(1, ep as i32)],
            )
        })
        .collect();
    let start = store::work_counters::user_summary_writes();
    store.publish_files(&scope, &pubs(&files), 3_000).unwrap();
    let ms = store::work_counters::user_summary_writes() - start;
    drop(store);
    let owned = ids
        .first()
        .map(|user| {
            rusqlite::Connection::open(tmp.path().join("library.db"))
                .unwrap()
                .query_row(
                    "SELECT owned_episode_count FROM user_library_items WHERE user_id=?1",
                    [user.to_string()],
                    |r| r.get(0),
                )
                .unwrap()
        })
        .unwrap_or(0);
    (ms, owned)
}

#[test]
fn batch_user_summary_cost_does_not_scale_with_files_times_users() {
    let (quiet, _) = batch(0);
    let (busy, owned) = batch(40);
    println!("summary writes for a 100-file batch: 0 users {quiet}, 40 users {busy}");
    assert_eq!(owned, 101, "summary reflects every file of the batch");
    assert_eq!(quiet, 0);
    assert_eq!(busy, 40, "one finalize per user per Media per batch");
}
