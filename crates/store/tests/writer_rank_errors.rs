use store::{Db, Store};

#[test]
fn inversion_and_reentry_are_errors_and_leave_the_writer_stack_healthy() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    for (held, requested) in [(Db::Library, Db::Subscribe), (Db::App, Db::App)] {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            store.with_writer(held, |_| {
                let error = store.with_writer(requested, |_| Ok(())).unwrap_err();
                let expected_held = match held { Db::App => store::WriterRank::App, _ => store::WriterRank::Library };
                let expected_requested = match requested { Db::App => store::WriterRank::App, _ => store::WriterRank::Subscribe };
                assert!(matches!(error, store::StoreError::LockOrder { held: ref stack, requested } if stack == &[expected_held] && requested == expected_requested), "{error:?}");
                Ok(())
            })
        }));
        assert!(
            outcome.is_ok(),
            "writer inversion must return an error, never panic"
        );
        outcome.unwrap().unwrap();
        store.with_writer(Db::App, |_| Ok(())).unwrap();
    }
    assert!(matches!(
        store.with_writer(Db::Subscribe, |_| {
            store.with_writer(Db::Library, |_| Ok(()))
        }),
        Err(store::StoreError::LockOrder { .. })
    ));
}

#[test]
fn jobs_rank_rejects_store_writer_and_unwind_cleans_up_held_stack() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    {
        let _jobs = store::WriterRankGuard::enter(store::WriterRank::Jobs).unwrap();
        assert!(matches!(store.with_writer(Db::Cache, |_| Ok(())),
            Err(store::StoreError::LockOrder { held, requested: store::WriterRank::Cache })
            if held == [store::WriterRank::Jobs]));
    }
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: Result<(), store::StoreError> =
            store.with_writer(Db::App, |_| panic!("fixture unwind"));
    }));
    assert!(panic.is_err());
    store.with_writer(Db::App, |_| Ok(())).unwrap();
}
#[test]
fn increasing_second_writer_is_rejected_before_waiting() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(tmp.path()).unwrap();
    store
        .with_writer(Db::Subscribe, |_| {
            assert!(
                matches!(
                    store.with_writer(Db::Library, |_| Ok(())),
                    Err(store::StoreError::LockOrder { .. })
                ),
                "any second actual writer must be rejected"
            );
            Ok(())
        })
        .unwrap();
}
