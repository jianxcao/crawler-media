//! A planned NFO export owns its sidecar path until it is terminal (review F4).
//!
//! `begin_nfo_export` used to delete any other pending intent on the same Media/path,
//! which left the older writer free to keep writing outside the lock: the disk kept
//! its bytes while the newer intent stayed pending and could not recover. Ownership is
//! now persisted (`library_operations.resource_key`, unique among live scrapes), so a
//! second intent on the same sidecar is refused instead of silently replacing one.
use domain::{EntityScope, Media, MediaId, MediaKind};
use store::{
    Store, StoreError,
    metadata::export_driver::{resume_nfo_export_phased, write_planned_nfo},
    test_support::{FixtureFile, publish_fixture_file},
};

struct Fixture {
    tmp: tempfile::TempDir,
    store: Store,
    video: std::path::PathBuf,
}

fn movie(store: &Store, title: &str) -> MediaId {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: title.into(),
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
    media.id
}

/// One Movie Library whose only file is `Movie.mkv`.
fn fixture() -> Fixture {
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
    let media = movie(&store, "Original");
    let video = root.join("Movie.mkv");
    std::fs::write(&video, b"movie").unwrap();
    publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media,
            path: video.to_string_lossy().into_owned(),
            ..Default::default()
        },
    )
    .unwrap();
    Fixture { tmp, store, video }
}

impl Fixture {
    fn sidecar(&self) -> std::path::PathBuf {
        self.video.with_extension("nfo")
    }

    fn target(&self, video: &std::path::Path, sidecar: &std::path::Path, op: &str) -> store::metadata::NfoExportRequest {
        self.store
            .nfo_export_target(video, sidecar, EntityScope::Work, op, &format!("export-{op}"))
            .unwrap()
            .unwrap()
    }

    fn plan(&self, operation_id: &str) -> store::metadata::NfoExportRequest {
        let request = self.target(&self.video, &self.sidecar(), operation_id);
        self.store.plan_nfo_export(&request, 1_000).unwrap()
    }
}

#[test]
fn active_export_keeps_exclusive_sidecar_ownership() {
    let fixture = fixture();
    fixture.plan("operation-a");
    let request_b = fixture.target(&fixture.video, &fixture.sidecar(), "operation-b");
    assert!(matches!(
        fixture.store.plan_nfo_export(&request_b, 2_000),
        Err(StoreError::Conflict(_))
    ));
    assert!(fixture.store.nfo_export_pending("operation-a").unwrap());
}

#[test]
fn another_media_cannot_claim_the_same_sidecar() {
    let fixture = fixture();
    fixture.plan("operation-a");
    let second = movie(&fixture.store, "Second");
    let other_video = fixture.video.with_file_name("Second.mkv");
    std::fs::write(&other_video, b"second").unwrap();
    publish_fixture_file(
        &fixture.store,
        &FixtureFile {
            media_id: second,
            path: other_video.to_string_lossy().into_owned(),
            ..Default::default()
        },
    )
    .unwrap();
    // A different Media writing the *same* sidecar file must not coexist either.
    let shared = fixture.target(&other_video, &fixture.sidecar(), "operation-b");
    assert!(matches!(
        fixture.store.plan_nfo_export(&shared, 2_000),
        Err(StoreError::Conflict(_))
    ));
}

#[test]
fn a_path_alias_does_not_claim_a_second_owner() {
    let fixture = fixture();
    fixture.plan("operation-a");
    let alias = fixture
        .video
        .parent()
        .unwrap()
        .join(".")
        .join("Movie.nfo");
    let request_b = fixture.target(&fixture.video, &alias, "operation-b");
    assert!(matches!(
        fixture.store.plan_nfo_export(&request_b, 2_000),
        Err(StoreError::Conflict(_))
    ));
}

#[test]
fn a_finished_export_releases_the_sidecar() {
    let fixture = fixture();
    let plan_a = fixture.plan("operation-a");
    let action = write_planned_nfo(&plan_a).unwrap();
    fixture
        .store
        .finish_planned_nfo_export(&plan_a, action, None, 1_100)
        .unwrap();
    assert!(!fixture.store.nfo_export_pending("operation-a").unwrap());
    assert!(fixture.store.plan_nfo_export(&fixture.target(&fixture.video, &fixture.sidecar(), "operation-b"), 1_200).is_ok());
}

#[test]
fn a_restarted_store_still_respects_the_live_owner() {
    let fixture = fixture();
    let plan_a = fixture.plan("operation-a");
    let data = fixture.tmp.path().join("data");
    let video = fixture.video.clone();
    let sidecar = fixture.sidecar();
    drop(fixture.store);
    let reopened = Store::open(data).unwrap();
    // Recovery drives the persisted intent; a second intent on the same path stays
    // refused until the first one is terminal.
    let request_b = reopened
        .nfo_export_target(&video, &sidecar, EntityScope::Work, "operation-b", "export-b")
        .unwrap()
        .unwrap();
    assert!(matches!(
        reopened.plan_nfo_export(&request_b, 2_000),
        Err(StoreError::Conflict(_))
    ));
    resume_nfo_export_phased(|| &reopened, &plan_a, 2_100).unwrap();
    assert!(reopened.plan_nfo_export(&request_b, 2_200).is_ok());
}

#[test]
fn an_external_edit_is_not_overwritten_by_a_stale_intent() {
    let fixture = fixture();
    let plan_a = fixture.plan("operation-a");
    // A third party rewrites the sidecar after the intent was planned.
    let edited = "<movie><title>Hand edited</title></movie>";
    std::fs::write(fixture.sidecar(), edited).unwrap();
    assert!(write_planned_nfo(&plan_a).is_err(), "a manual edit stays");
    assert_eq!(std::fs::read_to_string(fixture.sidecar()).unwrap(), edited);
}
