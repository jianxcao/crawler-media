//! Read-only Store operations remain available while the Library writer is held.
use domain::{EntityScope, MediaKind};
use std::{
    fmt::Debug,
    sync::{Arc, mpsc},
    time::Duration,
};
use store::{Db, FileReadScope, Store, StoreError};

#[path = "query_work/support.rs"]
#[allow(dead_code)]
mod support;

fn independent_read<T>(read: impl Fn(&Store) -> Result<T, StoreError> + Send + 'static)
where
    T: Debug + PartialEq + Send + 'static,
{
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    support::seed(&store, MediaKind::Movie, 1);
    let expected = read(&store).unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let writer_store = store.clone();
    let writer = std::thread::spawn(move || {
        writer_store
            .with_writer(Db::Library, |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(())
            })
            .unwrap();
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let (result_tx, result_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let result = read(&store);
        let _ = result_tx.send(result);
    });
    let result = result_rx.recv_timeout(Duration::from_millis(200));
    release_tx.send(()).unwrap();
    writer.join().unwrap();
    reader.join().unwrap();
    assert_eq!(
        result
            .expect("a read must not wait for the Library writer")
            .unwrap(),
        expected
    );
}

#[test]
fn page_hydration_does_not_acquire_the_library_writer() {
    independent_read(|store| {
        let library = store
            .list_libraries()?
            .into_iter()
            .find(|l| l.name == "Work")
            .unwrap();
        let files = store.file_coverage_in_library(
            library.id.parse().unwrap(),
            None,
            FileReadScope::AllRecorded,
        )?;
        let ids = files
            .iter()
            .map(|file| file.file.file_id)
            .collect::<Vec<_>>();
        store.page_hydration(&ids).map(|facts| facts.len())
    });
}

#[test]
fn watcher_presence_page_does_not_acquire_the_library_writer() {
    independent_read(|store| {
        store
            .watcher_presence_page("/query-work/movie", None, 100)
            .map(|rows| rows.len())
    });
}

#[test]
fn observable_watcher_work_does_not_acquire_the_library_writer() {
    independent_read(|store| {
        store
            .watcher_presence_page_with_work("/query-work/movie", None, 100)
            .map(|(rows, _, _)| rows.len())
    });
}

#[test]
fn effective_metadata_fields_does_not_acquire_the_library_writer() {
    independent_read(|store| {
        let library = store
            .list_libraries()?
            .into_iter()
            .find(|l| l.name == "Work")
            .unwrap();
        let file = store
            .file_coverage_in_library(
                library.id.parse().unwrap(),
                None,
                FileReadScope::AllRecorded,
            )?
            .remove(0);
        store
            .effective_metadata_fields(
                library.id.parse().unwrap(),
                file.file.media_id,
                EntityScope::Work,
                "zh-CN",
            )
            .map(|fields| fields.actual_locale)
    });
}

#[test]
fn pending_nfo_exports_does_not_acquire_the_library_writer() {
    independent_read(|store| store.pending_nfo_exports().map(|requests| requests.len()));
}
