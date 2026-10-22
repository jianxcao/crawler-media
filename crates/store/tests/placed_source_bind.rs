//! P03: first binding of a placed (Transfer) file's real source keeps facts
//! probed from the very same bytes by rebinding them, and invalidates them
//! otherwise — file, details and probe obligation in one transaction.
use std::path::{Path, PathBuf};

use domain::{FileVersion, LibraryFile, Media, MediaId, MediaKind};
use store::Store;
use store::library_write::{FilePublication, WriteScope};
use store::test_support::{FixtureFile, publish_fixture_file};

struct Fx {
    _guard: tempfile::TempDir,
    store: Store,
    root: PathBuf,
}

fn fixture() -> Fx {
    let guard = tempfile::tempdir().unwrap();
    let root = guard.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    let store = Store::open(guard.path().join("data")).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "M",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    Fx {
        _guard: guard,
        store,
        root,
    }
}

fn scope(fx: &Fx, file: &LibraryFile) -> WriteScope {
    WriteScope {
        library_id: file.library_id,
        config_revision: fx
            .store
            .library_config_revision(&file.library_id.to_string())
            .unwrap(),
    }
}

/// Reserve with a placeholder token (bytes not yet there), place the bytes and
/// publish with the placement stat evidence, like Transfer does.
fn placed(fx: &Fx, name: &str) -> (LibraryFile, library::SourceObservation) {
    let media = MediaId::new();
    fx.store
        .insert_media(&Media {
            id: media,
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
    let path = fx.root.join(name);
    let mut file = publish_fixture_file(
        &fx.store,
        &FixtureFile {
            media_id: media,
            path: path.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!path.exists());
    std::fs::write(&path, b"placed bytes").unwrap();
    let observation = library::observe_source(&path).unwrap();
    assert_ne!(
        observation.source_version, file.version.source_version,
        "placeholder token"
    );
    file.size_bytes = observation.size_bytes;
    file.modified_ns = observation.modified_ns;
    file.stat_known = true;
    let expected = file.version.clone();
    fx.store
        .publish_files(
            &scope(fx, &file),
            &[FilePublication {
                file: &file,
                units: &[domain::UnitKey::WHOLE],
                expected_version: Some(&expected),
                nfo_observation: None,
                probe_required: false,
            }],
            1,
        )
        .unwrap();
    (file, observation)
}

fn tracks() -> library::Tracks {
    library::Tracks {
        video: Some(library::VideoTrack {
            codec: Some("h264".into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn bind(fx: &Fx, file: &LibraryFile, observation: &library::SourceObservation) -> FileVersion {
    fx.store
        .bind_placed_file_source(
            &scope(fx, file),
            file.file_id,
            &file.version,
            observation,
            2,
        )
        .unwrap()
}

fn details_version(fx: &Fx, file: &LibraryFile) -> Option<(i64, String)> {
    let conn = rusqlite::Connection::open(fx.store.data_dir().join("library.db")).unwrap();
    conn.query_row(
        "SELECT source_revision, source_version FROM file_details WHERE file_id = ?1",
        [file.file_id.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .ok()
}

fn obligated(fx: &Fx, file: &LibraryFile) -> bool {
    fx.store
        .scan_probe_obligations(10)
        .unwrap()
        .iter()
        .any(|o| o.ledger_id == file.file_id)
}

#[test]
fn same_bytes_cold_row_is_rebound_and_stays_readable() {
    let fx = fixture();
    let (file, observation) = placed(&fx, "Same.2000.mkv");
    let id = file.file_id.to_string();
    fx.store.put_file_meta(&id, &tracks()).unwrap();
    fx.store.put_cached_chapters(&id, &[]).unwrap();
    assert!(fx.store.get_file_meta(&id).unwrap().is_some());

    let version = bind(&fx, &file, &observation);
    assert_eq!(version.revision, file.version.revision + 1);
    assert_eq!(version.source_version, observation.source_version);
    assert_eq!(
        details_version(&fx, &file),
        Some((version.revision, version.source_version.clone()))
    );
    assert!(
        fx.store.get_file_meta(&id).unwrap().is_some(),
        "same-bytes tracks stay readable"
    );
    assert!(
        fx.store.get_cached_chapters(&id).unwrap().is_some(),
        "chapters stay readable"
    );
    assert!(obligated(&fx, &file));
}

#[test]
fn no_cold_row_binds_and_rearms_probe() {
    let fx = fixture();
    let (file, observation) = placed(&fx, "Fresh.2000.mkv");
    let version = bind(&fx, &file, &observation);
    assert_eq!(version.source_version, observation.source_version);
    assert_eq!(details_version(&fx, &file), None);
    assert!(obligated(&fx, &file));
}

#[test]
fn bytes_changed_since_placement_invalidate_details() {
    let fx = fixture();
    let (file, _) = placed(&fx, "Changed.2000.mkv");
    let id = file.file_id.to_string();
    fx.store.put_file_meta(&id, &tracks()).unwrap();
    std::fs::write(&file.path, b"different, longer placed bytes").unwrap();
    let observation = library::observe_source(&file.path).unwrap();
    let version = bind(&fx, &file, &observation);
    assert_eq!(version.source_version, observation.source_version);
    assert_eq!(
        details_version(&fx, &file),
        None,
        "no evidence: details invalidated, not kept stale"
    );
    assert!(fx.store.get_file_meta(&id).unwrap().is_none());
    assert!(obligated(&fx, &file));
}

#[test]
fn stale_version_bind_is_rejected_without_partial_writes() {
    let fx = fixture();
    let (file, observation) = placed(&fx, "Stale.2000.mkv");
    let id = file.file_id.to_string();
    fx.store.put_file_meta(&id, &tracks()).unwrap();
    let mut stale = file.clone();
    stale.version.revision += 5;
    assert!(
        fx.store
            .bind_placed_file_source(
                &scope(&fx, &file),
                file.file_id,
                &stale.version,
                &observation,
                2
            )
            .is_err()
    );
    assert_eq!(
        details_version(&fx, &file),
        Some((file.version.revision, file.version.source_version.clone()))
    );
    // Idempotent retry with the real expected version binds exactly once.
    let version = bind(&fx, &file, &observation);
    assert_eq!(version.revision, file.version.revision + 1);
    assert!(
        fx.store
            .bind_placed_file_source(
                &scope(&fx, &file),
                file.file_id,
                &file.version,
                &observation,
                3
            )
            .is_err(),
        "a second bind with the consumed expected version is stale"
    );
    let _ = Path::new("");
}
