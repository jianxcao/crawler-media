#[path = "support/mod.rs"]
mod support;

use support::library_presence::PresenceFixture;

#[tokio::test]
async fn visible_scope_loads_libraries_owning_media() {
    let fix = PresenceFixture::new();

    let (m1, _) = fix.insert_media_and_ledger("vis1.mkv");
    let (m2, _) = fix.insert_media_and_ledger("vis2.mkv");

    let admin_id = {
        let store_arc = fix.state.store();
        let guard = store_arc.as_ref();
        guard.user_by_token(&fix.admin_token).unwrap().unwrap().id
    };

    let scope = {
        let store_arc = fix.state.store();
        let guard = store_arc.as_ref();
        api::visible_scope::visible_library_ids(guard, admin_id).unwrap()
    };

    let store = fix.state.store();
    for media in [m1, m2] {
        let coverage = store.file_coverage_for_media(media).unwrap();
        assert!(scope.contains(&coverage[0].file.library_id));
    }
}
