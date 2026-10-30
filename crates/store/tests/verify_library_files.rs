//! Verification observes filesystem state without owning a database connection.
use domain::{Media, MediaId, MediaKind};
use std::path::PathBuf;
use std::time::Duration;
use store::library_write::WriteScope;
use store::test_support::{FixtureFile, publish_fixture_file};
use store::{Db, Store};

struct Fixture {
    _temp: tempfile::TempDir,
    store: Store,
    root: PathBuf,
    file: domain::LibraryFile,
}
fn fixture() -> Fixture {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let root = temp.path().join("media");
    std::fs::create_dir_all(&root).unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Movie,
            title: "Verified movie".into(),
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
    let path = root.join("movie.mkv");
    std::fs::write(&path, b"finite source").unwrap();
    let file = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id,
            path: path.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    Fixture {
        _temp: temp,
        store,
        root,
        file,
    }
}

#[test]
fn verification_observes_files_without_a_library_reader_or_writer() {
    let fx = fixture();
    let result = fx
        .store
        .verify_library_files_with(std::slice::from_ref(&fx.root), 10, |path| {
            let acquired = std::thread::scope(|scope| {
                scope
                    .spawn(|| {
                        fx.store.writers().with_writer_timeout(
                            Duration::from_millis(100),
                            Db::Library,
                            |_| Ok(()),
                        )
                    })
                    .join()
                    .unwrap()
            });
            assert!(
                acquired.is_ok(),
                "filesystem observation must not own the Library writer"
            );
            let pool = fx.store.reader_pool(Db::Library);
            let readers = (0..8)
                .map(|_| pool.try_checkout().unwrap())
                .collect::<Vec<_>>();
            drop(readers);
            path.exists()
        })
        .unwrap();
    assert_eq!(result, (0, 0));
}

#[test]
fn verification_counts_missing_and_restored_only_once() {
    let fx = fixture();
    let roots = std::slice::from_ref(&fx.root);
    std::fs::remove_file(&fx.file.path).unwrap();
    assert_eq!(fx.store.verify_library_files(roots, 10).unwrap(), (1, 0));
    assert_eq!(fx.store.verify_library_files(roots, 11).unwrap(), (0, 0));
    let key = fx
        .store
        .recorded_path_key(&fx.file.path.display().to_string());
    assert_eq!(fx.store.missing_at_by_path().unwrap().get(&key), Some(&10));
    std::fs::write(&fx.file.path, b"restored source").unwrap();
    assert_eq!(fx.store.verify_library_files(roots, 12).unwrap(), (0, 1));
    assert_eq!(fx.store.verify_library_files(roots, 13).unwrap(), (0, 0));
    assert!(fx.store.missing_at_by_path().unwrap().is_empty());
}

#[test]
fn verification_discards_an_observation_after_the_recorded_path_changes() {
    let fx = fixture();
    let destination = fx.root.join("renamed.mkv");
    let result = fx
        .store
        .verify_library_files_with(std::slice::from_ref(&fx.root), 10, |_| {
            fx.store
                .rename_file_within_root(
                    fx.file.file_id,
                    &fx.file.version,
                    &fx.file.path,
                    &destination,
                )
                .unwrap();
            false
        })
        .unwrap();
    assert_eq!(result, (0, 0));
    assert_eq!(
        fx.store
            .file_coverage_by_id(fx.file.file_id)
            .unwrap()
            .unwrap()
            .file
            .path,
        destination
    );
    assert!(fx.store.missing_at_by_path().unwrap().is_empty());
}

#[test]
fn verification_discards_an_observation_after_the_source_version_changes() {
    let fx = fixture();
    let scope = WriteScope {
        library_id: fx.file.library_id,
        config_revision: fx
            .store
            .library_config_revision(&fx.file.library_id.to_string())
            .unwrap(),
    };
    let result = fx
        .store
        .verify_library_files_with(std::slice::from_ref(&fx.root), 10, |_| {
            std::fs::write(&fx.file.path, b"different observed content").unwrap();
            let observation = library::observe_source(&fx.file.path).unwrap();
            fx.store
                .replace_file_source(&scope, fx.file.file_id, &fx.file.version, &observation, 1)
                .unwrap();
            false
        })
        .unwrap();
    assert_eq!(result, (0, 0));
    assert_ne!(
        fx.store
            .file_coverage_by_id(fx.file.file_id)
            .unwrap()
            .unwrap()
            .file
            .version,
        fx.file.version
    );
    assert!(fx.store.missing_at_by_path().unwrap().is_empty());
}
