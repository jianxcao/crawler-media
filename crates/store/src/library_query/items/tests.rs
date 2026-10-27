//! Private connection hooks exercise the public browse seam without exposing test APIs.
use crate::{
    Store,
    library_query::{BrowseFilter, BrowseOrder, BrowsePage, LibraryQuery},
};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
#[path = "../../../tests/query_work/support.rs"]
#[allow(dead_code)]
mod support;

fn query(library_id: domain::LibraryId) -> LibraryQuery {
    LibraryQuery {
        library_id,
        user_id: None,
        filter: BrowseFilter::default(),
        order: BrowseOrder::Title,
        descending: false,
        page: BrowsePage::Offset {
            start: 0,
            limit: 10,
        },
        include_total: true,
    }
}

#[test]
fn browse_without_user_dependencies_never_reads_user_summary() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let library = support::seed(&store, domain::MediaKind::Movie, 4);
    store
        .library
        .authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
            AuthAction::Read {
                table_name: "user_library_items",
                ..
            } => Authorization::Deny,
            _ => Authorization::Allow,
        }));
    for user in [None, Some(domain::UserId::new())] {
        let mut q = query(library);
        q.user_id = user;
        assert_eq!(
            store.library_item_page_with_stats(&q).unwrap().0.total,
            Some(4)
        );
        assert_eq!(store.library_item_count(&q).unwrap(), 4);
        assert_eq!(store.library_item_titles(&q).unwrap().len(), 4);
        q.filter.genres = vec!["Drama".into(), "Comedy".into()];
        assert_eq!(store.library_item_page(&q).unwrap().total, Some(4));
    }
    for (order, watch_first, watch) in [
        (BrowseOrder::LastPlayed, false, None),
        (BrowseOrder::Title, true, None),
        (BrowseOrder::Title, false, Some("unwatched".into())),
    ] {
        let mut q = query(library);
        q.order = order;
        q.filter.unwatched_first = watch_first;
        q.filter.watch = watch;
        assert!(
            store.library_item_page(&q).is_err(),
            "user-state paths retain their required JOIN"
        );
    }
}

#[test]
fn browse_total_and_page_share_snapshot_during_committed_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let library = support::seed(&store, domain::MediaKind::Movie, 4);
    let writer = Store::open(dir.path()).unwrap();
    let (start_tx, start_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        start_rx.recv().unwrap();
        writer.library.execute("UPDATE library_files SET publication_state='pending' WHERE file_id=(SELECT file_id FROM library_files WHERE library_id=?1 LIMIT 1)",[library.to_string()]).unwrap();
        writer
            .begin_library_integrity(library, true, "snapshot-writer", 3000)
            .unwrap();
        while writer
            .advance_library_operation("snapshot-writer", 3001, 100)
            .unwrap()
            .state
            == "applying"
        {}
        done_tx.send(()).unwrap();
    });
    let mut triggered = false;
    store.library.authorizer(Some(move |ctx: AuthContext<'_>| {
        if !triggered
            && matches!(
                ctx.action,
                AuthAction::Read {
                    table_name: "library_items",
                    ..
                }
            )
        {
            triggered = true;
            start_tx.send(()).unwrap();
            done_rx.recv().unwrap();
        }
        Authorization::Allow
    }));
    let (page, work) = store.library_item_page_with_stats(&query(library)).unwrap();
    worker.join().unwrap();
    assert_eq!(page.total, Some(4));
    assert_eq!(page.items.len(), 4);
    assert!(work.vm_steps > 0);
    store
        .library
        .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
    assert_eq!(
        store.library_item_page(&query(library)).unwrap().total,
        Some(3)
    );
}
