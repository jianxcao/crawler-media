//! Admission-time classification of a NEW file (`classify_new_file` /
//! `Store::library_for_path_strict`): longest-root prefix, no default fallback.
//! A published file's owner is its recorded `library_files.library_id`, never a
//! path; that rule lives in `library_explicit_ownership` in the api crate.

use std::path::{Path, PathBuf};

use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource};
use store::test_support::FixtureFile;
use store::{Library, Store};

fn create(store: &Store, name: &str, roots: &[&Path]) -> Library {
    let roots: Vec<&str> = roots.iter().map(|root| root.to_str().unwrap()).collect();
    store
        .create_library(MediaKind::Movie, name, &roots, "everyone", true, &[])
        .unwrap()
}

fn ledger_row(media_id: MediaId, path: &Path) -> FixtureFile {
    FixtureFile {
        file_id: Some(LedgerId::new()),
        media_id,
        path: path.display().to_string(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
        ..Default::default()
    }
}

fn media() -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Same".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

#[test]
fn nested_root_wins_over_parent_root() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let parent_root = tmp.path().join("owned");
    let child_root = parent_root.join("child");
    let parent = create(&store, "parent", &[&parent_root]);
    let child = create(&store, "child", &[&child_root]);

    let libraries = store.list_libraries().unwrap();
    let parent_path = parent_root.join("one.mkv");
    let child_path = child_root.join("two.mkv");
    assert!(
        child_path.starts_with(&parent_root),
        "the nested path lies under the parent root too, so the longest root has to decide"
    );

    assert_eq!(
        store::classify_new_file(&libraries, &parent_path, MediaKind::Movie).map(|owner| &owner.id),
        Some(&parent.id)
    );
    assert_eq!(
        store::classify_new_file(&libraries, &child_path, MediaKind::Movie).map(|owner| &owner.id),
        Some(&child.id),
        "a nested root wins over its parent root"
    );
    assert_eq!(
        store
            .library_for_path_strict(&child_path, MediaKind::Movie)
            .unwrap()
            .unwrap()
            .id,
        child.id
    );
}

#[test]
fn unmatched_path_is_unclassified_without_a_default_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let default = store.default_library(MediaKind::Movie).unwrap().unwrap();
    let outside = tmp.path().join("elsewhere/one.mkv");

    let view = store.library_ownership().unwrap();
    // The snapshot still names the kind's default Library for "no Library bound
    // yet" callers (a Subscribe without a Library), but classification never
    // reaches for it.
    assert_eq!(view.default_for(MediaKind::Movie).unwrap().id, default.id);
    let libraries = store.list_libraries().unwrap();
    assert!(
        store::classify_new_file(&libraries, &outside, MediaKind::Movie).is_none(),
        "a path outside every root has no Library"
    );
    assert!(
        store
            .library_for_path_strict(&outside, MediaKind::Movie)
            .unwrap()
            .is_none()
    );
}

#[test]
fn sibling_prefix_path_does_not_match_root() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let owned_root = tmp.path().join("owned");
    let lib = create(&store, "owned", &[&owned_root]);

    let libraries = store.list_libraries().unwrap();
    let sibling = tmp.path().join("owned-other/one.mkv");
    assert!(
        store::classify_new_file(&libraries, &sibling, MediaKind::Movie).is_none(),
        "a sibling directory is not inside the root, and nothing defaults to {}",
        lib.id
    );
}

#[test]
fn same_media_in_two_libraries_owns_each_path_separately() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let parent_root = tmp.path().join("parent");
    let child_root = parent_root.join("child");
    let parent = create(&store, "parent", &[&parent_root]);
    let child = create(&store, "child", &[&child_root]);
    let media = media();
    store.insert_media(&media).unwrap();
    let parent_path = parent_root.join("one.mkv");
    let child_path = child_root.join("two.mkv");
    let r1 = ledger_row(media.id, &parent_path);
    let r2 = ledger_row(media.id, &child_path);
    let published_parent = store::test_support::publish_fixture_file(&store, &r1).unwrap();
    let published_child = store::test_support::publish_fixture_file(&store, &r2).unwrap();

    assert_eq!(published_parent.library_id.to_string(), parent.id);
    assert_eq!(published_child.library_id.to_string(), child.id);

    let in_parent = store
        .file_coverage_in_library(
            parent.id.parse().unwrap(),
            Some(media.id),
            store::FileReadScope::AllRecorded,
        )
        .unwrap();
    assert_eq!(in_parent.len(), 1);
    assert_eq!(in_parent[0].file.path, parent_path);
    let in_child = store
        .file_coverage_in_library(
            child.id.parse().unwrap(),
            Some(media.id),
            store::FileReadScope::AllRecorded,
        )
        .unwrap();
    assert_eq!(in_child.len(), 1);
    assert_eq!(in_child[0].file.path, child_path);
}

#[test]
fn snapshot_accepts_work_matches_store_helper_per_library() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    create(&store, "extra", &[&tmp.path().join("extra")]);

    let view = store.library_ownership().unwrap();
    assert!(!view.libraries().is_empty());
    for library in view.libraries() {
        assert_eq!(
            view.accepts_work(&library.id),
            store.library_accepts_work(&library.id).unwrap(),
            "library {}",
            library.id
        );
    }
    assert!(!view.accepts_work("missing-library"));
    assert!(!store.library_accepts_work("missing-library").unwrap());
}

#[test]
fn blocked_library_rejects_work_even_when_removal_succeeded() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let lib = create(&store, "frozen", &[&tmp.path().join("frozen")]);
    let conn = rusqlite::Connection::open(tmp.path().join("data/library.db")).unwrap();
    conn.execute(
        "INSERT INTO library_removals
            (operation_id, library_id, state, config_revision, removed_ledger_count,
             created_at_ms, updated_at_ms)
         VALUES ('op-succeeded', ?1, 'succeeded', 1, 0, 0, 0)",
        rusqlite::params![lib.id],
    )
    .unwrap();
    drop(conn);

    let view = store.library_ownership().unwrap();
    assert!(view.library(&lib.id).is_some());
    assert!(!view.accepts_work(&lib.id));
    assert!(!store.library_accepts_work(&lib.id).unwrap());
}

#[test]
fn snapshot_preserves_root_order_not_lexical_order() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let z = tmp.path().join("zroot");
    let a = tmp.path().join("aroot");
    let m = tmp.path().join("mroot");
    let lib = create(&store, "ordered", &[&z, &a, &m]);
    let expected: Vec<PathBuf> = vec![z, a, m];
    assert_eq!(lib.root_paths, expected);

    let view = store.library_ownership().unwrap();
    assert_eq!(view.library(&lib.id).unwrap().root_paths, expected);
    assert_eq!(
        store.get_library(&lib.id).unwrap().unwrap().root_paths,
        expected
    );
}
