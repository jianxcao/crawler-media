//! The current Transfer setting is one atomic, revisioned app.db fact.
#[path = "support/library_presence.rs"]
mod fixture;
use axum::http::StatusCode;
use fixture::PresenceFixture;
use serde_json::json;

#[tokio::test]
async fn failed_transfer_setting_write_preserves_snapshot_and_roots() {
    let f = PresenceFixture::new();
    let roots = f.state.store().lock().list_library_roots().unwrap();
    let previous = f.state.store().lock().transfer_settings().unwrap();
    let db = rusqlite::Connection::open(f.dir.path().join("data/app.db")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_transfer BEFORE INSERT ON settings WHEN NEW.key='transfer.settings' BEGIN SELECT RAISE(ABORT,'fixture failure'); END;").unwrap();
    let (status, _) = f.request_with_body("PATCH", "/api/v1/settings/transfer", Some(&f.admin_token), json!({"expected_revision":0,"transfer_mode":"copy","watch_intake":"/incoming","scrape":true})).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        f.state.store().lock().transfer_settings().unwrap(),
        previous
    );
    assert_eq!(f.state.store().lock().list_library_roots().unwrap(), roots);
}
