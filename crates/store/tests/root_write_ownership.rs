use std::path::{Path, PathBuf};

use domain::{LedgerId, LibraryFile, Media, MediaId, MediaKind, UnitKey};
use store::library_write::{FilePublication, WriteScope};
use store::test_support::{FixtureFile, publish_fixture_file};
use store::{Library, Store};

fn library(store: &Store, kind: MediaKind, name: &str, root: &Path) -> Library {
    store
        .create_library(kind, name, &[root.to_str().unwrap()], "everyone", true, &[])
        .unwrap()
}

fn movie(store: &Store, path: &Path) -> LibraryFile {
    let id = MediaId::new();
    store
        .insert_media(&Media {
            id,
            kind: MediaKind::Movie,
            title: "Movie".into(),
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
    std::fs::write(path, b"movie").unwrap();
    publish_fixture_file(
        store,
        &FixtureFile {
            media_id: id,
            path: path.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap()
}

fn publish(store: &Store, scope: &WriteScope, file: &LibraryFile) -> Result<(), store::StoreError> {
    store.publish_files(
        scope,
        &[FilePublication {
            file,
            units: &[UnitKey::WHOLE],
            expected_version: None,
            nfo_observation: None,
            probe_required: false,
        }],
        1,
    )?;
    Ok(())
}

fn old_scope_cannot_publish_into_nested_root(relative: bool) {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap().join("movies");
    let nested = root.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let parent = library(&store, MediaKind::Movie, "Parent", &root);
    let mut prepared = movie(&store, &nested.join("existing.mkv"));
    let scope = WriteScope {
        library_id: prepared.library_id,
        config_revision: store.library_config_revision(&parent.id).unwrap(),
    };
    let destination = library(&store, MediaKind::Movie, "Nested", &nested);
    prepared.file_id = LedgerId::new();
    prepared.path = if relative {
        PathBuf::from("nested/new.mkv")
    } else {
        nested.join("new.mkv")
    };
    prepared.version.source_version = "new-file".into();
    assert!(publish(&store, &scope, &prepared).is_err());
    assert!(
        store
            .file_coverage_by_id(prepared.file_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store::test_support::library_file_count(&store, &parent.id),
        0
    );
    assert_eq!(
        store::test_support::library_file_count(&store, &destination.id),
        1
    );
}

#[test]
fn absolute_publication_rejects_a_parent_scope_after_nested_root_creation() {
    old_scope_cannot_publish_into_nested_root(false);
}

#[test]
fn relative_publication_rejects_a_parent_scope_after_nested_root_creation() {
    old_scope_cannot_publish_into_nested_root(true);
}

#[test]
fn a_nested_root_of_another_kind_does_not_steal_movie_ownership() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap().join("movies");
    let nested = root.join("tv");
    std::fs::create_dir_all(&nested).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let parent = library(&store, MediaKind::Movie, "Movies", &root);
    library(&store, MediaKind::Tv, "TV", &nested);
    let file = movie(&store, &nested.join("movie.mkv"));
    assert_eq!(file.library_id.to_string(), parent.id);
    assert_eq!(
        store
            .file_coverage_by_id(file.file_id)
            .unwrap()
            .unwrap()
            .file
            .library_id,
        file.library_id
    );
}

#[cfg(unix)]
#[test]
fn a_missing_root_keeps_its_identity_when_created_under_an_alias() {
    let tmp = tempfile::tempdir().unwrap();
    let physical = tmp.path().join("physical");
    let alias = tmp.path().join("alias");
    std::fs::create_dir(&physical).unwrap();
    std::os::unix::fs::symlink(&physical, &alias).unwrap();
    let root = alias.join("movies");
    let store = Store::open(tmp.path().join("data")).unwrap();
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
    std::fs::create_dir(&root).unwrap();
    let path = root.join("Movie.mkv");
    std::fs::write(&path, b"movie").unwrap();
    let actual = std::fs::canonicalize(path).unwrap();
    let owner = store
        .library_root_owning_key(&actual, MediaKind::Movie)
        .unwrap();
    assert_eq!(owner.map(|(id, _)| id), Some(library.id));
}
