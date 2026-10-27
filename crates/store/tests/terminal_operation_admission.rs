//! Terminal operations must not block admission (review F3).
//!
//! A `failed` scrape is finished business: recovery never resumes it, retention drops
//! it, and `unfinished_library_operations` does not list it. Admission used to refuse
//! every row whose state was not `succeeded`, so one failed export could block root,
//! locale, integrity, removal and default-language changes for the retention window.
use domain::{EntityScope, Media, MediaId, MediaKind};
use store::{
    Store,
    library_operations::LibraryReconfiguration,
    test_support::{FixtureFile, publish_fixture_file},
};

struct Fixture {
    tmp: tempfile::TempDir,
    store: Store,
    library_id: domain::LibraryId,
    video: std::path::PathBuf,
}

/// A Library owning one movie file, with no operation of its own.
fn library_fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("movies");
    std::fs::create_dir(&root).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
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
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Original".into(),
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
    let video = root.join("Movie.mkv");
    std::fs::write(&video, b"movie").unwrap();
    publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media.id,
            path: video.to_string_lossy().into_owned(),
            ..Default::default()
        },
    )
    .unwrap();
    let library_id = store
        .file_coverage_by_path(video.to_str().unwrap())
        .unwrap()
        .unwrap()
        .file
        .library_id;
    Fixture {
        tmp,
        store,
        library_id,
        video,
    }
}

/// The same Library with one NFO export intent already planned.
fn fixture() -> Fixture {
    let fixture = library_fixture();
    let nfo = fixture.video.with_extension("nfo");
    let request = fixture
        .store
        .nfo_export_target(&fixture.video, &nfo, EntityScope::Work, "scrape-op", "export-a")
        .unwrap()
        .unwrap();
    fixture.store.plan_nfo_export(&request, 1_000).unwrap();
    fixture
}

impl Fixture {
    /// Finish the scrape as a permanent failure: the terminal state under test.
    fn fail_scrape(&self) {
        self.store
            .fail_nfo_export("scrape-op", "source changed", 2_000)
            .unwrap();
        assert!(
            !self
                .store
                .unfinished_library_operations()
                .unwrap()
                .contains(&"scrape-op".into()),
            "a failed scrape is not an unfinished operation"
        );
    }

    /// A second, empty root the Library may additionally own.
    fn extra_root(&self) -> std::path::PathBuf {
        let root = self.tmp.path().join("movies-extra");
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn reconfiguration(&self, operation_id: &str) -> LibraryReconfiguration {
        LibraryReconfiguration {
            operation_id: operation_id.into(),
            library_id: self.library_id,
            expected_revision: self
                .store
                .library_config_revision(&self.library_id.to_string())
                .unwrap(),
            root_paths: None,
            display_locale: None,
        }
    }

    fn begin_locale(&self) -> Result<(), store::StoreError> {
        let mut request = self.reconfiguration("locale-op");
        request.display_locale = Some("en-US".into());
        self.store
            .begin_library_reconfiguration(&request, 3_000)
            .map(|_| ())
    }
}

#[test]
fn failed_scrape_does_not_block_locale_reconfiguration() {
    let fixture = fixture();
    fixture.fail_scrape();
    assert!(fixture.begin_locale().is_ok());
}

#[test]
fn failed_scrape_does_not_block_root_reconfiguration() {
    let fixture = fixture();
    fixture.fail_scrape();
    let mut request = fixture.reconfiguration("roots-op");
    request.root_paths = Some(vec![
        fixture.video.parent().unwrap().to_path_buf(),
        fixture.extra_root(),
    ]);
    let begun = fixture.store.begin_library_reconfiguration(&request, 3_000);
    assert!(begun.is_ok(), "{begun:?}");
}

#[test]
fn failed_scrape_does_not_block_integrity() {
    let fixture = fixture();
    fixture.fail_scrape();
    let status = fixture
        .store
        .begin_library_integrity(fixture.library_id, false, "integrity-op", 3_000)
        .unwrap();
    assert_ne!(status.state, "failed");
}

#[test]
fn failed_scrape_does_not_block_library_removal() {
    let fixture = fixture();
    fixture.fail_scrape();
    // A second Library of the same kind keeps the removal legal (the last one may not go).
    let other = fixture.tmp.path().join("movies-2");
    std::fs::create_dir(&other).unwrap();
    fixture
        .store
        .create_library(
            MediaKind::Movie,
            "Movies 2",
            &[other.to_str().unwrap()],
            "everyone",
            false,
            &[],
        )
        .unwrap();
    let removal = fixture
        .store
        .begin_library_removal(&fixture.library_id.to_string())
        .unwrap();
    assert_eq!(removal.library_id, fixture.library_id.to_string());
}

#[test]
fn live_scrape_still_blocks_admission() {
    let fixture = fixture();
    // The intent stays planned: it is still resumable work, so admission must refuse.
    assert!(
        fixture.begin_locale().is_err(),
        "a live scrape still owns the library"
    );
}

#[test]
fn frozen_library_operation_still_blocks_admission() {
    let fixture = library_fixture();
    let mut request = fixture.reconfiguration("roots-op");
    request.root_paths = Some(vec![
        fixture.video.parent().unwrap().to_path_buf(),
        fixture.extra_root(),
    ]);
    fixture
        .store
        .begin_library_reconfiguration(&request, 3_000)
        .unwrap();
    // A resumable operation (frozen, not terminal) keeps the Library owned.
    assert!(
        fixture.begin_locale().is_err(),
        "an unfinished operation still owns the library"
    );
}
