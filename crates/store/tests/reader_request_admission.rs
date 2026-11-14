//! Cross-database snapshots cannot hold readers in conflicting orders.
use std::{
    num::NonZeroUsize,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use store::{Db, Store, StoreError};

#[test]
fn opposite_read_orders_complete_without_reader_pool_deadlock() {
    let root = std::path::Path::new("/tmp/crawler-storage-fixes");
    std::fs::create_dir_all(root).unwrap();
    let dir = tempfile::tempdir_in(root).unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    for db in [Db::Library, Db::App] {
        store
            .configure_reader_pool_size(db, NonZeroUsize::new(1).unwrap())
            .unwrap();
    }
    let user = domain::UserId::new();
    let (entered, ready) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let first = {
        let store = store.clone();
        std::thread::spawn(move || {
            store.with_read_snapshot(&[Db::App, Db::Library], Duration::from_secs(2), || {
                store.list_libraries()?;
                entered.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(3)).unwrap();
                store.user_role(user)?;
                Ok::<_, StoreError>(())
            })
        })
    };
    ready.recv_timeout(Duration::from_secs(3)).unwrap();
    let second = {
        let store = store.clone();
        std::thread::spawn(move || {
            store.with_read_snapshot(&[Db::App, Db::Library], Duration::from_secs(2), || {
                store.user_role(user)?;
                store.list_libraries()?;
                Ok::<_, StoreError>(())
            })
        })
    };
    let library = store.reader_pool(Db::Library);
    let deadline = Instant::now() + Duration::from_secs(1);
    while library.waiting() == 0 && Instant::now() < deadline {
        std::thread::yield_now();
    }
    let observed_wait = library.waiting() > 0;
    release.send(()).unwrap();
    let first = first.join().unwrap();
    let second = second.join().unwrap();
    assert!(
        observed_wait,
        "second read stage must reach Library admission"
    );
    assert!(first.is_ok(), "first read stage failed: {first:?}");
    assert!(first.unwrap().is_ok());
    assert!(second.is_ok(), "second read stage failed: {second:?}");
    assert!(second.unwrap().is_ok());
    for db in [Db::Library, Db::App] {
        assert_eq!(store.reader_pool(db).available(), 1);
    }
}

#[test]
fn undeclared_database_is_rejected_even_when_the_callback_ignores_the_error() {
    let root = std::path::Path::new("/tmp/crawler-storage-fixes");
    std::fs::create_dir_all(root).unwrap();
    let dir = tempfile::tempdir_in(root).unwrap();
    let store = Store::open(dir.path()).unwrap();
    let result = store.with_read_snapshot(&[Db::Library], Duration::from_secs(1), || {
        store.list_libraries().unwrap();
        assert!(matches!(
            store.user_role(domain::UserId::new()),
            Err(StoreError::Protected(_))
        ));
    });
    assert!(matches!(result, Err(StoreError::Protected(_))));
    assert_eq!(store.reader_pool(Db::Library).available(), 1);
    assert_eq!(store.reader_pool(Db::App).available(), 1);
}

#[test]
fn nested_declarations_may_narrow_but_cannot_expand_the_outer_stage() {
    let root = std::path::Path::new("/tmp/crawler-storage-fixes");
    std::fs::create_dir_all(root).unwrap();
    let dir = tempfile::tempdir_in(root).unwrap();
    let store = Store::open(dir.path()).unwrap();
    let user = domain::UserId::new();
    store
        .with_read_snapshot(&[Db::Library], Duration::from_secs(1), || {
            let expanded =
                store.with_read_snapshot(&[Db::Library, Db::App], Duration::from_secs(1), || {
                    panic!("expanded declaration ran")
                });
            assert!(matches!(expanded, Err(StoreError::Protected(_))));
            assert!(!store.list_libraries().unwrap().is_empty());
        })
        .unwrap();
    store
        .with_read_snapshot(&[Db::Library, Db::App], Duration::from_secs(1), || {
            let narrowed = store.with_read_snapshot(&[Db::Library], Duration::from_secs(1), || {
                let _ = store.user_role(user);
            });
            assert!(matches!(narrowed, Err(StoreError::Protected(_))));
            assert_eq!(store.user_role(user).unwrap(), "member");
        })
        .unwrap();
}

#[test]
fn externally_bound_readers_enforce_the_same_database_order() {
    let root = std::path::Path::new("/tmp/crawler-storage-fixes");
    std::fs::create_dir_all(root).unwrap();
    let dir = tempfile::tempdir_in(root).unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lease = store.reader_pool(Db::App).try_checkout().unwrap();
    let reversed = store
        .with_reader_lease(Db::App, lease, |store| store.list_libraries())
        .unwrap();
    assert!(matches!(reversed, Err(StoreError::Protected(_))));
    let lease = store.reader_pool(Db::Library).try_checkout().unwrap();
    let role = store
        .with_reader_lease(Db::Library, lease, |store| {
            store.user_role(domain::UserId::new())
        })
        .unwrap()
        .unwrap();
    assert_eq!(role, "member");
}

#[test]
fn library_only_stages_keep_all_eight_readers_without_borrowing_other_databases() {
    let root = std::path::Path::new("/tmp/crawler-storage-fixes");
    std::fs::create_dir_all(root).unwrap();
    let dir = tempfile::tempdir_in(root).unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let mut other_leases = Vec::new();
    for db in [Db::App, Db::Subscribe, Db::Cache] {
        store
            .configure_reader_pool_size(db, NonZeroUsize::new(1).unwrap())
            .unwrap();
        other_leases.push(store.reader_pool(db).try_checkout().unwrap());
    }
    let gate = Arc::new((parking_lot::Mutex::new(false), parking_lot::Condvar::new()));
    let (entered, ready) = mpsc::channel();
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let store = store.clone();
            let entered = entered.clone();
            let gate = gate.clone();
            std::thread::spawn(move || {
                store.with_read_snapshot(&[Db::Library], Duration::from_secs(5), || {
                    store.list_libraries().unwrap();
                    entered.send(()).unwrap();
                    let mut released = gate.0.lock();
                    gate.1.wait_while_for(
                        &mut released,
                        |released| !*released,
                        Duration::from_secs(3),
                    );
                    assert!(*released, "test must release all Library readers");
                })
            })
        })
        .collect();
    let mut simultaneous = 0;
    while simultaneous < 8 && ready.recv_timeout(Duration::from_secs(1)).is_ok() {
        simultaneous += 1;
    }
    *gate.0.lock() = true;
    gate.1.notify_all();
    for worker in workers {
        worker.join().unwrap().unwrap();
    }
    assert_eq!(
        simultaneous, 8,
        "pure Library reads must retain full capacity"
    );
    assert_eq!(store.reader_pool(Db::Library).available(), 8);
    drop(other_leases);
}
