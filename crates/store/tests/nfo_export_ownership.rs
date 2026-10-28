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
    metadata::export_driver::{
        export_nfo_sidecar_phased, observe_nfo_sidecar, resume_nfo_export_phased,
    },
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

    fn target(
        &self,
        video: &std::path::Path,
        sidecar: &std::path::Path,
        op: &str,
    ) -> store::metadata::NfoExportRequest {
        self.store
            .nfo_export_target(
                video,
                sidecar,
                EntityScope::Work,
                op,
                &format!("export-{op}"),
            )
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
    let alias = fixture.video.parent().unwrap().join(".").join("Movie.nfo");
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
    resume_nfo_export_phased(|| &fixture.store, &plan_a, 1_100).unwrap();
    assert!(!fixture.store.nfo_export_pending("operation-a").unwrap());
    assert!(
        fixture
            .store
            .plan_nfo_export(
                &fixture.target(&fixture.video, &fixture.sidecar(), "operation-b"),
                1_200
            )
            .is_ok()
    );
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
        .nfo_export_target(
            &video,
            &sidecar,
            EntityScope::Work,
            "operation-b",
            "export-b",
        )
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
    assert!(
        resume_nfo_export_phased(|| &fixture.store, &plan_a, 1_100).is_err(),
        "a manual edit stays"
    );
    assert_eq!(std::fs::read_to_string(fixture.sidecar()).unwrap(), edited);
}

#[test]
fn recovery_releases_unreplayable_first_export_and_preserves_manual_fields_on_retry() {
    let fixture = fixture();
    let plan = fixture.plan("operation-a");
    let edited = "<movie><title>Hand edited</title></movie>";
    std::fs::write(fixture.sidecar(), edited).unwrap();
    assert!(resume_nfo_export_phased(|| &fixture.store, &plan, 1_100).is_err());
    assert!(!fixture.store.nfo_export_pending("operation-a").unwrap());
    assert_eq!(std::fs::read_to_string(fixture.sidecar()).unwrap(), edited);

    let mut retry = fixture.target(&fixture.video, &fixture.sidecar(), "operation-b");
    let observation = observe_nfo_sidecar(&fixture.sidecar(), EntityScope::Work).unwrap();
    retry.local_edits = fixture
        .store
        .nfo_local_edits_observed(&retry, &observation)
        .unwrap()
        .unwrap();
    export_nfo_sidecar_phased(|| &fixture.store, &retry, 1_200).unwrap();
    let output = std::fs::read_to_string(fixture.sidecar()).unwrap();
    assert!(output.contains("<title>Hand edited</title>"), "{output}");
}

#[test]
fn recovery_releases_invalid_sidecar_without_overwriting_it() {
    let fixture = fixture();
    let plan = fixture.plan("operation-a");
    std::fs::write(fixture.sidecar(), b"not an nfo").unwrap();
    assert!(resume_nfo_export_phased(|| &fixture.store, &plan, 1_100).is_err());
    assert!(!fixture.store.nfo_export_pending("operation-a").unwrap());
    assert_eq!(std::fs::read(fixture.sidecar()).unwrap(), b"not an nfo");
    assert!(
        fixture
            .store
            .plan_nfo_export(
                &fixture.target(&fixture.video, &fixture.sidecar(), "operation-b"),
                1_200
            )
            .is_ok()
    );
}

#[test]
fn recovery_releases_owner_when_external_edit_and_file_version_changed() {
    let fixture = fixture();
    let first = fixture.target(&fixture.video, &fixture.sidecar(), "operation-a");
    export_nfo_sidecar_phased(|| &fixture.store, &first, 1_000).unwrap();
    let plan = fixture.plan("operation-b");
    let edited = "<movie><title>Manual title</title></movie>";
    std::fs::write(fixture.sidecar(), edited).unwrap();
    let conn = rusqlite::Connection::open(fixture.tmp.path().join("data/library.db")).unwrap();
    conn.execute(
        "UPDATE library_files SET file_revision = file_revision + 1 WHERE file_id = ?1",
        [plan.file_id.to_string()],
    )
    .unwrap();

    assert!(resume_nfo_export_phased(|| &fixture.store, &plan, 1_100).is_err());
    assert!(!fixture.store.nfo_export_pending("operation-b").unwrap());
    assert_eq!(std::fs::read_to_string(fixture.sidecar()).unwrap(), edited);
    let next = fixture.target(&fixture.video, &fixture.sidecar(), "operation-c");
    assert!(fixture.store.plan_nfo_export(&next, 1_200).is_ok());
}

#[test]
fn io_failure_releases_owner_and_allows_a_later_export_after_reopen() {
    let fixture = fixture();
    let plan = fixture.plan("operation-a");
    std::fs::create_dir(fixture.sidecar()).unwrap();
    assert!(resume_nfo_export_phased(|| &fixture.store, &plan, 1_100).is_err());
    assert!(!fixture.store.nfo_export_pending("operation-a").unwrap());
    assert!(
        fixture
            .store
            .nfo_export_registration("operation-a")
            .unwrap()
            .is_none()
    );
    std::fs::remove_dir(fixture.sidecar()).unwrap();
    let data = fixture.tmp.path().join("data");
    let video = fixture.video.clone();
    let sidecar = fixture.sidecar();
    drop(fixture.store);
    let store = Store::open(data).unwrap();
    assert!(!store.nfo_export_pending("operation-a").unwrap());
    let retry = store
        .nfo_export_target(
            &video,
            &sidecar,
            EntityScope::Work,
            "operation-b",
            "export-b",
        )
        .unwrap()
        .unwrap();
    export_nfo_sidecar_phased(|| &store, &retry, 1_200).unwrap();
    assert!(
        store
            .nfo_export_registration("operation-b")
            .unwrap()
            .is_some()
    );
}

#[test]
fn written_sidecar_registration_retries_a_transient_database_failure() {
    let fixture = fixture();
    let plan = fixture.plan("operation-a");
    std::fs::write(&plan.path, &plan.body).unwrap();
    let database = fixture.tmp.path().join("data/library.db");
    let conn = rusqlite::Connection::open(&database).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_export_completion BEFORE UPDATE OF state ON library_operations WHEN NEW.state = 'succeeded' BEGIN SELECT RAISE(ABORT, 'temporary failure'); END;").unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(75));
        let conn = rusqlite::Connection::open(database).unwrap();
        conn.execute_batch("DROP TRIGGER reject_export_completion")
            .unwrap();
    });
    let outcome = resume_nfo_export_phased(|| &fixture.store, &plan, 1_100);
    release.join().unwrap();
    outcome.unwrap();
    assert!(
        fixture
            .store
            .nfo_export_registration("operation-a")
            .unwrap()
            .is_some()
    );
}

#[test]
fn exhausted_registration_failure_releases_owner_without_success_baseline() {
    let fixture = fixture();
    let plan = fixture.plan("operation-a");
    std::fs::write(&plan.path, &plan.body).unwrap();
    let conn = rusqlite::Connection::open(fixture.tmp.path().join("data/library.db")).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_export_completion BEFORE UPDATE OF state ON library_operations WHEN NEW.state = 'succeeded' BEGIN SELECT RAISE(ABORT, 'persistent failure'); END;").unwrap();
    assert!(resume_nfo_export_phased(|| &fixture.store, &plan, 1_100).is_err());
    assert!(!fixture.store.nfo_export_pending("operation-a").unwrap());
    assert!(
        fixture
            .store
            .nfo_export_registration("operation-a")
            .unwrap()
            .is_none()
    );
    conn.execute_batch("DROP TRIGGER reject_export_completion")
        .unwrap();
    let edited = "<movie><title>Hand edited after failure</title></movie>";
    std::fs::write(fixture.sidecar(), edited).unwrap();
    let mut retry = fixture.target(&fixture.video, &fixture.sidecar(), "operation-b");
    let observation = observe_nfo_sidecar(&fixture.sidecar(), EntityScope::Work).unwrap();
    retry.local_edits = fixture
        .store
        .nfo_local_edits_observed(&retry, &observation)
        .unwrap()
        .unwrap();
    export_nfo_sidecar_phased(|| &fixture.store, &retry, 1_200).unwrap();
    assert!(
        std::fs::read_to_string(fixture.sidecar())
            .unwrap()
            .contains("<title>Hand edited after failure</title>")
    );
}

#[test]
fn stale_recovery_cannot_write_after_a_new_owner_is_planned() {
    let fixture = fixture();
    let old = fixture.plan("operation-a");
    std::fs::write(fixture.sidecar(), b"external invalid nfo").unwrap();
    assert!(resume_nfo_export_phased(|| &fixture.store, &old, 1_100).is_err());
    std::fs::remove_file(fixture.sidecar()).unwrap();
    let new = fixture.plan("operation-b");
    assert!(resume_nfo_export_phased(|| &fixture.store, &old, 1_200).is_err());
    assert!(!fixture.sidecar().exists());
    assert!(fixture.store.nfo_export_pending("operation-b").unwrap());
    resume_nfo_export_phased(|| &fixture.store, &new, 1_300).unwrap();
}

#[test]
fn failed_registration_preserves_previous_baseline_after_reopen() {
    let fixture = fixture();
    let first = fixture.target(&fixture.video, &fixture.sidecar(), "operation-a");
    export_nfo_sidecar_phased(|| &fixture.store, &first, 1_000).unwrap();
    let next = fixture.plan("operation-b");
    std::fs::write(&next.path, &next.body).unwrap();
    let database = fixture.tmp.path().join("data/library.db");
    let conn = rusqlite::Connection::open(&database).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_export_completion BEFORE UPDATE OF state ON library_operations WHEN NEW.state = 'succeeded' BEGIN SELECT RAISE(ABORT, 'persistent failure'); END;").unwrap();
    assert!(resume_nfo_export_phased(|| &fixture.store, &next, 1_100).is_err());
    conn.execute_batch("DROP TRIGGER reject_export_completion")
        .unwrap();
    let manual = "<movie><title>Changed after failure</title><plot>Keep this too</plot></movie>";
    std::fs::write(fixture.sidecar(), manual).unwrap();
    let video = fixture.video.clone();
    let sidecar = fixture.sidecar();
    let data = fixture.tmp.path().join("data");
    drop(fixture.store);
    let store = Store::open(data).unwrap();
    assert!(!store.nfo_export_pending("operation-b").unwrap());
    let mut retry = store
        .nfo_export_target(
            &video,
            &sidecar,
            EntityScope::Work,
            "operation-c",
            "export-c",
        )
        .unwrap()
        .unwrap();
    let observed = observe_nfo_sidecar(&sidecar, EntityScope::Work).unwrap();
    retry.local_edits = store
        .nfo_local_edits_observed(&retry, &observed)
        .unwrap()
        .unwrap();
    export_nfo_sidecar_phased(|| &store, &retry, 1_200).unwrap();
    let output = std::fs::read_to_string(sidecar).unwrap();
    assert!(
        output.contains("<title>Changed after failure</title>"),
        "{output}"
    );
    assert!(output.contains("<plot>Keep this too</plot>"), "{output}");
}

#[test]
fn recovery_cannot_run_while_original_writer_is_registering() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let fixture = fixture();
    let request = fixture.target(&fixture.video, &fixture.sidecar(), "operation-a");
    let shared = Arc::new(std::sync::Mutex::new(fixture.store));
    let worker_store = shared.clone();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release = std::sync::Mutex::new(release_rx);
    let calls = AtomicUsize::new(0);
    let worker = std::thread::spawn(move || {
        export_nfo_sidecar_phased(
            || {
                if calls.fetch_add(1, Ordering::SeqCst) == 2 {
                    entered_tx.send(()).unwrap();
                    release.lock().unwrap().recv().unwrap();
                }
                worker_store.lock().unwrap()
            },
            &request,
            1_000,
        )
    });
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let plan = shared
        .lock()
        .unwrap()
        .pending_nfo_exports()
        .unwrap()
        .pop()
        .unwrap();
    let outcome = resume_nfo_export_phased(|| shared.lock().unwrap(), &plan, 1_100);
    let cancelled = shared
        .lock()
        .unwrap()
        .fail_nfo_export("operation-a", "cancel", 1_100);
    release_tx.send(()).unwrap();
    let original = worker.join().unwrap();
    assert!(matches!(outcome, Err(StoreError::Conflict(_))));
    assert!(matches!(cancelled, Err(StoreError::Conflict(_))));
    original.unwrap();
    assert!(
        shared
            .lock()
            .unwrap()
            .nfo_export_registration("operation-a")
            .unwrap()
            .is_some()
    );
}
