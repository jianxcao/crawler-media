use super::*;

#[tokio::test]
async fn missing_session_identity_is_rejected_without_fabricating_facts() {
    let fix = PresenceFixture::new();
    let (_, file) = fix.insert_media_and_ledger("unbound.mkv");
    for event in ["start", "progress", "stop"] {
        let (status, _) = fix
            .request_with_body(
                "POST",
                "/api/v1/playback/progress",
                Some(&fix.admin_token),
                json!({
                    "device_id":"unbound", "media_item_id":file.media_id.to_string(),
                    "file_id":file.file_id.to_string(), "event":event, "position_ms":3000,
                    "event_sequence":1
                }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let db = rusqlite::Connection::open(fix.dir.path().join("data/library.db")).unwrap();
    for table in [
        "playback_units",
        "playback_sessions",
        "playback_logs",
        "playback_session_ids",
    ] {
        let count: i64 = db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
}
