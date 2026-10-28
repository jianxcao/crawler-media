//! Item 1 (2026-10-29 E2E): deleted sources leave nothing behind — no sidecars,
//! no empty directories, no orphan Media rows — and a rescan that finds a file
//! gone removes it instead of only flagging it missing.
mod support;

use axum::http::StatusCode;
use support::library_presence::PresenceFixture;

#[tokio::test]
async fn rescan_purges_vanished_file_media_and_sidecars() {
    let fix = PresenceFixture::new();
    let dir = fix.root.join("Gone (2020)");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Gone (2020).strm"), b"http://x").unwrap();
    let (media_id, row) = fix.insert_media_and_ledger("Gone (2020)/Gone (2020).strm");
    for name in ["Gone (2020).nfo", "poster.jpg", "fanart.jpg"] {
        std::fs::write(dir.join(name), b"x").unwrap();
    }
    std::fs::remove_file(&row.path).unwrap();

    let (status, _) = fix.scan_and_wait(&fix.library_id).await;
    assert_eq!(status, StatusCode::OK);

    let store = fix.state.store();
    let guard = store.lock();
    assert!(
        guard.file_coverage_by_id(row.file_id).unwrap().is_none(),
        "vanished file row must be removed by the rescan, not flagged missing"
    );
    assert!(
        guard.get_media(media_id).unwrap().is_none(),
        "orphan Media purged"
    );
    assert!(
        !dir.exists(),
        "sidecars and the empty directory are removed"
    );
    assert!(fix.root.exists(), "the Library root itself stays");
}

#[tokio::test]
async fn deleting_an_item_removes_its_sidecars_and_directory() {
    let fix = PresenceFixture::new();
    let dir = fix.root.join("Film (2023)");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Film (2023).strm"), b"http://x").unwrap();
    let (media_id, _row) = fix.insert_media_and_ledger("Film (2023)/Film (2023).strm");
    for name in ["Film (2023).nfo", "poster.jpg", "clearlogo.png"] {
        std::fs::write(dir.join(name), b"x").unwrap();
    }

    let (status, body) = fix
        .request(
            "DELETE",
            &format!("/api/v1/libraries/{}/items/{media_id}", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["rows_deleted"], 1);
    assert!(
        !dir.exists(),
        "sidecars and the empty directory are removed"
    );
    assert!(fix.root.exists());
    assert!(
        fix.state
            .store()
            .lock()
            .get_media(media_id)
            .unwrap()
            .is_none()
    );
}
