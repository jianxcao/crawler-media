//! D-H2: the client already has the same torrent (same info hash) without our
//! tags, e.g. added by hand. Submission claims it instead of failing forever.

use super::harness::*;
use super::mock_qbit::Task;

const INFO: &str = "d6:lengthi12e4:name39:The.Matrix.1999.2160p.BluRay.x265-GROUP12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaae";

#[tokio::test]
async fn existing_same_infohash_task_is_claimed_and_imported() {
    let h = Harness::new();
    h.create_site_at_mock().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    let torrent = format!("d8:announce17:https://t.ex/anno4:info{INFO}e");
    let hash = downloader::torrent_info_hash(torrent.as_bytes()).unwrap();
    *h.qb.torrent_file.lock() = Some(torrent);
    h.qb.inject(Task {
        hash: hash.clone(),
        name: "user-added".into(),
        tags: vec!["mine".into()],
        progress: 0.4,
        save_path: h.qb.save_root.display().to_string(),
        files: vec![],
        size: 1,
        state: "downloading".into(),
        source: "manual".into(),
    });

    let (status, body) = h.run_subscribe_raw(id).await;
    assert_eq!(status, axum::http::StatusCode::BAD_GATEWAY, "{body}");
    assert!(body.to_string().contains("existing_foreign"), "{body}");
    assert_eq!(h.qb.add_count(), 0, "no duplicate add");
    assert_eq!(h.qb.add_tags.lock().len(), 0, "never addTags foreign task");
    let task = h.qb.tasks.lock()[0].clone();
    assert_eq!(task.tags, vec!["mine".to_string()], "user tags untouched");
    assert_eq!(h.pending_state(id, "active"), 0);
    let own = h
        .state
        .store()
        .find_download_ownership_by_hash(domain::DownloaderId::from_uuid(uuid::Uuid::nil()), &hash)
        .unwrap();
    assert!(own.iter().any(
        |r| r.ownership_state == store::DownloadOwnershipState::Foreign
            && r.provenance_kind == store::DownloadProvenanceKind::ExistingForeign
    ));
}
