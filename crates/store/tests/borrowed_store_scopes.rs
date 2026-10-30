//! Same-connection transactions and the Subscribe/Library reference boundary.
use domain::{Coverage, FetchMode, Media, MediaId, MediaKind, Subscribe, SubscribeId, UserId};
use std::sync::{Arc, Barrier};
use store::{FileReadScope, Store, StoreError};
#[path = "query_work/support.rs"]
#[allow(dead_code)]
mod support;
fn media() -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Borrowed scope".into(),
        year: None,
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}
#[test]
fn library_write_reads_its_own_writes_and_rolls_back_a_failed_scope() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(dir.path()).unwrap();
    let committed = media();
    store
        .library_write(|library| {
            library.insert_media(&committed)?;
            assert_eq!(library.get_media(committed.id)?, Some(committed.clone()));
            let mut revised = committed.clone();
            revised.title = "Revised inside one scope".into();
            library.update_media(&revised)?;
            assert_eq!(library.get_media(revised.id)?, Some(revised));
            Ok(())
        })
        .unwrap();
    assert_eq!(
        store.get_media(committed.id).unwrap().unwrap().title,
        "Revised inside one scope"
    );
    let refused = media();
    let result: Result<(), StoreError> = store.library_write(|library| {
        library.insert_media(&refused)?;
        assert!(library.get_media(refused.id)?.is_some());
        Err(StoreError::Protected("fixture refusal".into()))
    });
    assert!(result.is_err());
    assert!(store.get_media(refused.id).unwrap().is_none());
    assert!(store.get_media(committed.id).unwrap().is_some());
}
fn subscription(store: &Store, media_id: MediaId) -> Subscribe {
    Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id,
        coverage: Coverage::Movie,
        fetch_mode: FetchMode::Search,
        filter_id: store.list_filters().unwrap()[0].id,
        wash_cut: false,
        wash_cut_filter_id: None,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
        keep_old_versions: false,
    }
}
#[test]
fn concurrent_subscribe_creation_and_last_file_deletion_never_leave_a_dangling_reference() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let library = support::seed(&store, MediaKind::Movie, 12);
    let files = store
        .file_coverage_in_library(library, None, FileReadScope::AllRecorded)
        .unwrap();
    for file in files {
        let subscribe = subscription(&store, file.file.media_id);
        let gate = Arc::new(Barrier::new(3));
        let creating_store = store.clone();
        let creating_gate = gate.clone();
        let creating_subscribe = subscribe.clone();
        let create = std::thread::spawn(move || {
            creating_gate.wait();
            creating_store.insert_subscribe(&creating_subscribe)
        });
        let deleting_store = store.clone();
        let deleting_gate = gate.clone();
        let file_id = file.file.file_id;
        let delete = std::thread::spawn(move || {
            deleting_gate.wait();
            deleting_store.delete_files(library, &[file_id])
        });
        gate.wait();
        let created = create.join().unwrap();
        assert_eq!(delete.join().unwrap().unwrap(), 1);
        let referenced = store
            .list_all_subscribes()
            .unwrap()
            .iter()
            .any(|s| s.id == subscribe.id);
        let exists = store.get_media(subscribe.media_id).unwrap().is_some();
        assert_eq!(referenced, created.is_ok());
        assert_eq!(
            exists, referenced,
            "last-file purge raced Subscribe creation"
        );
    }
}

#[test]
fn rejected_probe_batch_rolls_back_only_its_reservation_inside_a_successful_scope() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(dir.path()).unwrap();
    let library = support::seed(&store, MediaKind::Movie, 2);
    let files = store
        .file_coverage_in_library(library, None, FileReadScope::AllRecorded)
        .unwrap();
    let ledger_ids: Vec<_> = files
        .iter()
        .map(|file| file.file.file_id.to_string())
        .collect();
    let specs: Vec<_> = ledger_ids
        .iter()
        .map(|ledger_id| store::ProbeJobUnitSpec {
            ledger_id,
            kind: "movie",
            force_fingerprint: false,
            reuse_fingerprint_cache: false,
            overwrite_markers: false,
            reuse_media_info_cache: false,
        })
        .collect();
    let media_id = files[0].file.media_id.to_string();
    assert!(
        store
            .create_probe_job(
                "occupied",
                "media_probe",
                &media_id,
                None,
                "occupied-scope",
                &specs[1..]
            )
            .unwrap()
    );
    let unrelated = media();
    store
        .library_write(|connection| {
            connection.insert_media(&unrelated)?;
            assert!(!connection.create_probe_job(
                "refused",
                "media_probe",
                &media_id,
                None,
                "refused-scope",
                &specs,
            )?);
            assert!(
                connection
                    .active_probe_job_for_scope("refused-scope")?
                    .is_none()
            );
            assert!(
                connection
                    .active_probe_unit_for_ledger(&ledger_ids[0])?
                    .is_none()
            );
            Ok(())
        })
        .unwrap();
    assert!(
        store
            .active_probe_job_for_scope("refused-scope")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .active_probe_unit_for_ledger(&ledger_ids[0])
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .active_probe_unit_for_ledger(&ledger_ids[1])
            .unwrap()
            .unwrap()
            .job_id,
        "occupied"
    );
    assert_eq!(store.get_media(unrelated.id).unwrap(), Some(unrelated));
}

#[test]
fn cancelled_write_scope_rolls_back_and_releases_the_writer_for_a_visible_commit() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(dir.path()).unwrap();
    let refused = media();
    let budget = store::QueryBudget {
        deadline: std::time::Instant::now() + std::time::Duration::from_secs(5),
        cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let result = store.with_query_budget(&budget, |store| {
        store
            .library_write(|library| {
                library.insert_media(&refused)?;
                budget
                    .cancelled
                    .store(true, std::sync::atomic::Ordering::Release);
                Ok(())
            })
            .map_err(Into::into)
    });
    assert!(matches!(result, Err(store::LibraryReadError::Cancelled)));
    let committed = media();
    store.insert_media(&committed).unwrap();
    assert_eq!(
        store.get_media(committed.id).unwrap(),
        Some(committed),
        "a cancelled transaction must not capture the next writer's commit"
    );
    assert!(store.get_media(refused.id).unwrap().is_none());
}

#[test]
fn panicking_cancelled_write_scope_rolls_back_before_the_writer_is_reused() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(dir.path()).unwrap();
    let refused = media();
    let budget = store::QueryBudget {
        deadline: std::time::Instant::now() + std::time::Duration::from_secs(5),
        cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        store.with_query_budget(&budget, |store| {
            store
                .library_write(|library| -> Result<(), StoreError> {
                    library.insert_media(&refused)?;
                    budget
                        .cancelled
                        .store(true, std::sync::atomic::Ordering::Release);
                    panic!("cancelled writer fixture panic");
                })
                .map_err(Into::into)
        })
    }));
    assert!(result.is_err());
    let committed = media();
    store.insert_media(&committed).unwrap();
    assert_eq!(
        store.get_media(committed.id).unwrap(),
        Some(committed),
        "a panicking cancelled transaction must not capture the next writer's commit"
    );
    assert!(store.get_media(refused.id).unwrap().is_none());
}
