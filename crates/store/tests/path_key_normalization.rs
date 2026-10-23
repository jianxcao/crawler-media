//! Risk 1: `path_key` is the stable normalized identity used for uniqueness and
//! lookup; `display_path` keeps the caller's spelling. Existing rows whose key was
//! written verbatim are migrated on open without losing data.
use domain::{Media, MediaId, MediaKind};
use std::path::PathBuf;
use store::Store;
use store::test_support::{FixtureFile, publish_fixture_file};

fn movie(store: &Store) -> MediaId {
    let id = MediaId::new();
    store
        .insert_media(&Media {
            id,
            kind: MediaKind::Movie,
            title: "M".into(),
            year: Some(2000),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    id
}

fn setup() -> (tempfile::TempDir, Store, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("movies");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "A",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    (tmp, store, root)
}

fn raw(tmp: &tempfile::TempDir) -> rusqlite::Connection {
    rusqlite::Connection::open(tmp.path().join("data").join("library.db")).unwrap()
}

fn file_rows(tmp: &tempfile::TempDir) -> Vec<(String, String, String)> {
    let conn = raw(tmp);
    let mut stmt = conn
        .prepare("SELECT file_id, path_key, display_path FROM library_files ORDER BY file_id")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn equivalent_spellings_share_one_key_and_keep_display_spelling() {
    let (tmp, store, root) = setup();
    let media = movie(&store);
    let clean = root.join("sub").join("M.2000.mkv");
    std::fs::write(&clean, b"m").unwrap();
    let odd = format!("{}//./sub/M.2000.mkv", root.display());
    let first = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media,
            path: odd.clone(),
            ..Default::default()
        },
    )
    .unwrap();
    let rows = file_rows(&tmp);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, clean.display().to_string(), "key is normalized");
    assert_eq!(rows[0].2, odd, "display keeps the caller spelling");

    let second = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media,
            path: clean.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        first.file_id, second.file_id,
        "same normalized key is the same file"
    );
    assert_eq!(file_rows(&tmp).len(), 1);
}

#[test]
fn root_paths_differing_only_by_spelling_are_duplicates() {
    let (_tmp, store, root) = setup();
    let spelled = format!("{}/", root.display());
    let dup = store.create_library(
        MediaKind::Movie,
        "B",
        &[spelled.as_str()],
        "everyone",
        true,
        &[],
    );
    assert!(
        dup.is_err(),
        "trailing-slash spelling must collide: {dup:?}"
    );
}

#[test]
fn schema_rejects_non_canonical_keys_directly() {
    let (tmp, store, root) = setup();
    let media = movie(&store);
    let clean = root.join("A.mkv");
    std::fs::write(&clean, b"m").unwrap();
    publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media,
            path: clean.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    drop(store);
    let conn = raw(&tmp);
    for bad in [
        format!("{}//A.mkv", root.display()),
        format!("{}/./A.mkv", root.display()),
        format!("{}/x/../A.mkv", root.display()),
        format!("{}/", root.display()),
        "relative/A.mkv".to_string(),
    ] {
        let result = conn.execute("UPDATE library_files SET path_key = ?1", [&bad]);
        assert!(
            result.is_err(),
            "schema must reject non-canonical key {bad:?}"
        );
    }
}

#[test]
fn nfd_and_nfc_spellings_are_one_key() {
    let (tmp, store, root) = setup();
    let media = movie(&store);
    let nfd = root.join("Cafe\u{301}.mkv");
    std::fs::write(&nfd, b"m").unwrap();
    let first = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media,
            path: nfd.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    let nfc = root.join("Caf\u{e9}.mkv");
    let second = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media,
            path: nfc.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(first.file_id, second.file_id);
    let rows = file_rows(&tmp);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, nfc.display().to_string(), "key is NFC");
}

#[test]
fn symlinked_root_is_keyed_canonically_and_aliases_collide() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    let alias = tmp.path().join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "A",
            &[alias.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = movie(&store);
    let via_alias = alias.join("M.mkv");
    std::fs::write(&via_alias, b"m").unwrap();
    publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media,
            path: via_alias.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    let rows = file_rows(&tmp);
    let real_key = std::fs::canonicalize(&real).unwrap().join("M.mkv");
    assert_eq!(
        rows[0].1,
        real_key.display().to_string(),
        "file keyed under canonical root"
    );
    assert_eq!(
        rows[0].2,
        via_alias.display().to_string(),
        "display keeps alias spelling"
    );
    let dup = store.create_library(
        MediaKind::Movie,
        "B",
        &[real.to_str().unwrap()],
        "everyone",
        true,
        &[],
    );
    assert!(
        dup.is_err(),
        "the real path of an aliased root is the same root: {dup:?}"
    );
    let dotted = format!("{}/x/..", real.display());
    std::fs::create_dir_all(real.join("x")).unwrap();
    let dup = store.create_library(
        MediaKind::Movie,
        "C",
        &[dotted.as_str()],
        "everyone",
        true,
        &[],
    );
    assert!(
        dup.is_err(),
        "`..` spelling of an existing root resolves to it: {dup:?}"
    );
    let missing = format!("{}/nope/../y", tmp.path().display());
    let bad = store.create_library(
        MediaKind::Movie,
        "D",
        &[missing.as_str()],
        "everyone",
        true,
        &[],
    );
    assert!(
        bad.is_err(),
        "`..` in a non-existent root is rejected: {bad:?}"
    );
}

#[test]
fn file_paths_with_parent_components_are_rejected() {
    let (_tmp, store, root) = setup();
    let media = movie(&store);
    std::fs::write(root.join("A.mkv"), b"m").unwrap();
    let dotted = format!("{}/sub/../A.mkv", root.display());
    let result = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media,
            path: dotted,
            ..Default::default()
        },
    );
    assert!(result.is_err(), "{result:?}");
}

/// R02: a nested Library added through a symlink alias takes over the files it now owns.
/// Ownership compares the canonical `path_key` on component boundaries; display spelling
/// never decides, and a sibling directory sharing a name prefix stays put.
#[test]
fn nested_symlink_root_reassigns_existing_files() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    std::fs::create_dir_all(real.join("nested")).unwrap();
    std::fs::create_dir_all(real.join("nested-sibling")).unwrap();
    let alias = tmp.path().join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let a = store
        .create_library(
            MediaKind::Movie,
            "A",
            &[alias.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = movie(&store);
    let nested_file = alias.join("nested").join("N.mkv");
    let sibling_file = alias.join("nested-sibling").join("S.mkv");
    std::fs::write(&nested_file, b"nested bytes").unwrap();
    std::fs::write(&sibling_file, b"sibling").unwrap();
    for path in [&nested_file, &sibling_file] {
        publish_fixture_file(
            &store,
            &FixtureFile {
                media_id: media,
                path: path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
    }
    let bytes_before = std::fs::read(&nested_file).unwrap();
    let nested_root = alias.join("nested");
    let b = store
        .create_library(
            MediaKind::Movie,
            "B",
            &[nested_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let conn = raw(&tmp);
    let owner = |name: &str| -> String {
        conn.query_row(
            "SELECT library_id FROM library_files WHERE display_path LIKE ?1",
            [format!("%/{name}")],
            |r| r.get(0),
        )
        .unwrap()
    };
    let strict_owner = store
        .library_for_path_strict(&nested_file, MediaKind::Movie)
        .unwrap()
        .expect("strict resolver owner");
    assert_eq!(strict_owner.id, b.id);
    assert_eq!(
        owner("N.mkv"),
        strict_owner.id,
        "persisted owner matches resolver"
    );
    assert_eq!(
        owner("S.mkv"),
        a.id,
        "prefix sibling is not under the nested root"
    );
    let card_count = |library: &str| -> i64 {
        conn.query_row(
            "SELECT COALESCE(SUM(file_count), 0) FROM library_items WHERE library_id = ?1",
            [library],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(card_count(&b.id), 1);
    assert_eq!(card_count(&a.id), 1);
    assert_eq!(std::fs::read(&nested_file).unwrap(), bytes_before);
}
