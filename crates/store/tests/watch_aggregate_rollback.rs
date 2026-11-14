use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use store::Store;

#[test]
fn watch_report_failure_releases_its_library_snapshot() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(tmp.path()).unwrap();
    store
        .configure_reader_pool_size(store::Db::Library, std::num::NonZeroUsize::new(1).unwrap())
        .unwrap();

    for _ in 0..2 {
        store
            .with_read(store::Db::Library, |conn| {
                conn.authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
                    AuthAction::Read {
                        table_name: "playback_logs",
                        ..
                    } => Authorization::Deny,
                    _ => Authorization::Allow,
                }));
                Ok(())
            })
            .unwrap();
        let error = store
            .playback_watch_report(None, None, 1_700_000_000, 30, 0)
            .unwrap_err();
        assert!(error.to_string().contains("playback_logs"), "{error}");
        let next = store.with_read(store::Db::Library, |conn| {
            conn.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
            Ok(())
        });
        assert!(
            next.is_ok(),
            "failed report must release its library snapshot"
        );
        assert!(
            store
                .playback_watch_report(None, None, 1_700_000_000, 30, 0)
                .is_ok()
        );
    }
}
