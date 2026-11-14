//! B1: request reads scoped by persisted Library ownership and an explicit read scope.

use domain::{FilePresence, FilePublication, LedgerId, LibraryId, Media, MediaId, MediaKind};
use store::test_support::{FixtureFile, force_file_states, publish_fixture_file};
use store::{FileReadScope, Store};

fn library(store: &Store, root: &std::path::Path, name: &str) -> LibraryId {
    std::fs::create_dir_all(root).unwrap();
    let lib = store
        .create_library(
            MediaKind::Tv,
            name,
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    lib.id.parse().unwrap()
}

fn publish(store: &Store, media: MediaId, path: std::path::PathBuf, ep: u32) -> LedgerId {
    let id = LedgerId::new();
    publish_fixture_file(
        store,
        &FixtureFile {
            file_id: Some(id),
            media_id: media,
            path: path.display().to_string(),
            season: Some(1),
            episode: Some(ep),
            ..Default::default()
        },
    )
    .unwrap();
    id
}

fn ids(store: &Store, lib: LibraryId, media: MediaId, scope: FileReadScope) -> Vec<LedgerId> {
    let mut out: Vec<LedgerId> = store
        .file_coverage_in_library(lib, Some(media), scope)
        .unwrap()
        .into_iter()
        .map(|c| c.file.file_id)
        .collect();
    out.sort_by_key(|id| id.to_string());
    out
}

#[test]
fn scoped_coverage_keeps_presence_and_publication_semantics() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let a = library(&store, &tmp.path().join("a"), "A");
    let b = library(&store, &tmp.path().join("b"), "B");
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Show".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let present = publish(&store, media.id, tmp.path().join("a/p.mkv"), 1);
    let missing = publish(&store, media.id, tmp.path().join("a/m.mkv"), 2);
    let unknown = publish(&store, media.id, tmp.path().join("a/u.mkv"), 3);
    let pending = publish(&store, media.id, tmp.path().join("a/q.mkv"), 4);
    let hidden = publish(&store, media.id, tmp.path().join("b/h.mkv"), 1);
    force_file_states(
        &store,
        missing,
        FilePresence::Missing,
        FilePublication::Published,
    )
    .unwrap();
    force_file_states(
        &store,
        unknown,
        FilePresence::Unknown,
        FilePublication::Published,
    )
    .unwrap();
    force_file_states(
        &store,
        pending,
        FilePresence::Present,
        FilePublication::Pending,
    )
    .unwrap();

    let mut published = vec![present, missing, unknown];
    published.sort_by_key(|id| id.to_string());
    let mut all = vec![present, missing, unknown, pending];
    all.sort_by_key(|id| id.to_string());
    assert_eq!(
        ids(&store, a, media.id, FileReadScope::Playable),
        vec![present]
    );
    assert_eq!(
        ids(&store, a, media.id, FileReadScope::Published),
        published
    );
    assert_eq!(ids(&store, a, media.id, FileReadScope::AllRecorded), all);
    assert_eq!(
        ids(&store, b, media.id, FileReadScope::AllRecorded),
        vec![hidden]
    );

    // Ownership comes from the persisted row, never from path roots.
    let whole = store
        .file_coverage_in_library(a, None, FileReadScope::Published)
        .unwrap();
    assert!(whole.iter().all(|c| c.file.library_id == a));
}
