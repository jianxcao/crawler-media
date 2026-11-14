use api::library_lifecycle::LibraryWorkRegistry;
use std::sync::Arc;

#[tokio::test]
async fn freeze_cancels_existing_work_and_blocks_new_leases() {
    let temp = tempfile::tempdir().unwrap();
    let store = store::Store::open(temp.path()).unwrap();
    let root = temp.path().join("extra");
    let library = store
        .create_library(
            domain::MediaKind::Tv,
            "Extra",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let registry = Arc::new(LibraryWorkRegistry::default());
    let lease = registry.acquire(&store, &library.id).unwrap();
    registry.freeze(&store, &library.id).unwrap();
    assert!(lease.control().is_cancelled());
    assert!(registry.acquire(&store, &library.id).is_err());
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(10),
            registry.wait_idle(&library.id)
        )
        .await
        .is_err()
    );
    drop(lease);
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        registry.wait_idle(&library.id),
    )
    .await
    .unwrap();
}

const LOCK_CHILD_ENV: &str = "LIBRARY_WORK_LOCK_ORDER_CHILD";

/// Run `name` in a child process watched by an external deadline: a lock cycle
/// reports as a failure instead of wedging the test binary.
fn watched(name: &str) -> bool {
    if std::env::var(LOCK_CHILD_ENV).as_deref() == Ok(name) {
        return false;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([name, "--exact", "--nocapture", "--test-threads=1"])
        .env(LOCK_CHILD_ENV, name)
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "{name}: child failed: {status}");
            return true;
        }
        if started.elapsed() > std::time::Duration::from_secs(60) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{name}: lock cycle — child did not finish within 60s");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn two_libraries(store: &store::Store, temp: &std::path::Path) -> (store::Library, store::Library) {
    let make = |name: &str| {
        let root = temp.join(name);
        std::fs::create_dir_all(&root).unwrap();
        store
            .create_library(
                domain::MediaKind::Movie,
                name,
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
    };
    (make("source"), make("target"))
}

/// S02: a publication on the source holds its gate while a concurrent freeze of
/// the source is in progress; acquiring (and dropping) a target lease inside that
/// publication must not close a registry ↔ gate cycle.
#[test]
fn target_lease_inside_source_publication_does_not_deadlock_freeze() {
    const NAME: &str = "target_lease_inside_source_publication_does_not_deadlock_freeze";
    if watched(NAME) {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(store::Store::open(temp.path()).unwrap());
    let (source, target) = two_libraries(&store, temp.path());
    let registry = Arc::new(LibraryWorkRegistry::default());
    let lease = registry.acquire(&store, &source.id).unwrap();

    let (in_gate_tx, in_gate_rx) = std::sync::mpsc::channel();
    let (freeze_tx, freeze_rx) = std::sync::mpsc::channel();
    let (organize_tx, organize_rx) = std::sync::mpsc::channel();
    let freezer = {
        let (registry, store, id) = (registry.clone(), store.clone(), source.id.clone());
        std::thread::spawn(move || {
            in_gate_rx.recv().unwrap();
            let _ = freeze_tx.send(registry.freeze(&store, &id).is_ok());
        })
    };
    let organizer = {
        let (registry, store, source_id, target_id) = (
            registry.clone(),
            store.clone(),
            source.id.clone(),
            target.id.clone(),
        );
        std::thread::spawn(move || {
            let outcome = lease.control().publish(|| {
                in_gate_tx.send(()).unwrap();
                // Wait for the durable removal mark: the freeze is now committed
                // and about to cancel this very gate.
                while store.as_ref().library_accepts_work(&source_id).unwrap() {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                let target_lease = registry.acquire(&store, &target_id);
                drop(target_lease);
                Ok(())
            });
            drop(lease);
            let _ = organize_tx.send(outcome.is_ok());
        })
    };
    let deadline = std::time::Duration::from_secs(20);
    let organize_finished = organize_rx.recv_timeout(deadline).is_ok();
    let freeze_finished = freeze_rx.recv_timeout(deadline);
    if !organize_finished || freeze_finished.is_err() {
        eprintln!(
            "LOCK_CYCLE organize_finished={organize_finished} freeze_finished={}",
            freeze_finished.is_ok()
        );
        std::process::exit(3);
    }
    assert!(freeze_finished.unwrap());
    organizer.join().unwrap();
    freezer.join().unwrap();
    assert!(registry.acquire(&store, &source.id).is_err());
    assert!(registry.acquire(&store, &target.id).is_ok());
}

/// C01: `cancel` must not wait on a publication gate while it holds the registry;
/// the publication acquires and drops a target lease (registry) inside the gate.
#[test]
fn cancel_during_publication_allows_nested_lease_drop() {
    const NAME: &str = "cancel_during_publication_allows_nested_lease_drop";
    if watched(NAME) {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(store::Store::open(temp.path()).unwrap());
    let (source, target) = two_libraries(&store, temp.path());
    let registry = Arc::new(LibraryWorkRegistry::default());
    let lease = registry.acquire(&store, &source.id).unwrap();
    let old_control = lease.control().clone();

    let (in_gate_tx, in_gate_rx) = std::sync::mpsc::channel();
    let (cancel_tx, cancel_rx) = std::sync::mpsc::channel();
    let (publish_tx, publish_rx) = std::sync::mpsc::channel();
    let canceller = {
        let (registry, id) = (registry.clone(), source.id.clone());
        std::thread::spawn(move || {
            in_gate_rx.recv().unwrap();
            registry.cancel(&id);
            let _ = cancel_tx.send(());
        })
    };
    let publisher = {
        let (registry, store, target_id) = (registry.clone(), store.clone(), target.id.clone());
        std::thread::spawn(move || {
            let _ = lease.control().publish(|| {
                in_gate_tx.send(()).unwrap();
                // Scheduling checkpoint: let the canceller take the registry first.
                std::thread::sleep(std::time::Duration::from_millis(100));
                drop(registry.acquire(&store, &target_id));
                Ok(())
            });
            drop(lease);
            let _ = publish_tx.send(());
        })
    };
    let deadline = std::time::Duration::from_secs(20);
    let publisher_finished = publish_rx.recv_timeout(deadline).is_ok();
    let cancellation_finished = cancel_rx.recv_timeout(deadline).is_ok();
    if !(publisher_finished && cancellation_finished) {
        eprintln!("LOCK_CYCLE publisher={publisher_finished} cancel={cancellation_finished}");
        std::process::exit(3);
    }
    publisher.join().unwrap();
    canceller.join().unwrap();
    assert!(old_control.is_cancelled());
    assert!(registry.acquire(&store, &source.id).is_err());

    // A cancel with no entry still forbids a later acquire.
    let fresh = Arc::new(LibraryWorkRegistry::default());
    fresh.cancel(&target.id);
    assert!(fresh.acquire(&store, &target.id).is_err());
}
