//! G-M2: an already-owned qBittorrent task short-circuits the add. A broken
//! one (error / missingFiles) must not count as submitted: the enclosure is
//! rejected and the next round picks another source. A paused one is resumed.

use super::harness::*;
use super::mock_qbit::Task;

fn stub_hash_for(enclosure: &str) -> String {
    let parsed_url = url::Url::parse(enclosure).unwrap();
    let mut target = parsed_url.path().to_string();
    if let Some(q) = parsed_url.query() {
        target.push('?');
        target.push_str(q);
    }
    let announce = format!("http://127.0.0.1{target}");
    let bytes = format!(
        "d8:announce{}:{announce}4:infod6:lengthi1e4:name{}:{target}12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaaee",
        announce.len(),
        target.len()
    );
    downloader::torrent_info_hash(bytes.as_bytes()).unwrap()
}

fn owned_task(state: &str, enclosure: &str, op_tag: &str) -> Task {
    Task {
        hash: stub_hash_for(enclosure),
        name: MATRIX_TITLE.into(),
        tags: vec![
            "crawler-media".into(),
            downloader::ownership_tag(enclosure),
            op_tag.into(),
        ],
        progress: 0.3,
        save_path: "/downloads".into(),
        files: Vec::new(),
        size: 1,
        state: state.into(),
        source: enclosure.into(),
    }
}

async fn broken_existing_task_is_rejected_and_replaced(state: &str) {
    let h = Harness::new();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    h.qb.inject(owned_task(state, &h.enclosure(1), "cm-op:broken-1"));
    h.set_search_rows(&[(MATRIX_TITLE, 1)]);
    h.scheduled_search(id).await.unwrap();
    assert_eq!(
        h.pending_state(id, "active"),
        0,
        "{state}: broken task is not an active pending"
    );
    assert_eq!(
        h.pending_state(id, "rejected"),
        1,
        "{state}: enclosure rejected"
    );

    let other = "The.Matrix.1999.1080p.BluRay.x264-OTHER";
    h.set_search_rows(&[(MATRIX_TITLE, 1), (other, 2)]);
    h.scheduled_search(id).await.unwrap();
    let active: Vec<_> = h
        .pending(id)
        .into_iter()
        .map(|(_, p)| p.torrent.enclosure)
        .collect();
    assert_eq!(
        active,
        vec![h.enclosure(2)],
        "{state}: next round switches source"
    );
}

#[tokio::test]
async fn error_task_is_rejected() {
    broken_existing_task_is_rejected_and_replaced("error").await;
}

#[tokio::test]
async fn missing_files_task_is_rejected() {
    broken_existing_task_is_rejected_and_replaced("missingFiles").await;
}

#[tokio::test]
async fn paused_task_is_resumed_and_pending_stays_active() {
    let h = Harness::new();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    let enc = h.enclosure(1);
    let hash = stub_hash_for(&enc);
    let dl_id = domain::DownloaderId::from_uuid(uuid::Uuid::nil());
    let op_id = uuid::Uuid::new_v4().to_string();
    let op_tag = format!("cm-op:{op_id}");
    h.state
        .store()
        .accept_download_operation(&store::AcceptDownloadOperationInput {
            operation_id: op_id.clone(),
            idempotency_key: "paused-op-1".into(),
            source_kind: "subscribe".into(),
            subscribe_id: Some(id),
            plugin_id: None,
            subject: "system".into(),
            cause_id: "paused-cause".into(),
            site_id: None,
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            enclosure: enc.clone(),
            ownership_tag: op_tag.clone(),
            info_hash: Some(hash.clone()),
            bytes_digest: None,
            verified_bytes: Some(1024),
            request_digest: "req-paused".into(),
            now_utc: 1,
        })
        .unwrap();
    h.state
        .store()
        .record_download_ownership(&store::RecordDownloadOwnershipInput {
            downloader_id: dl_id,
            ownership_tag: op_tag.clone(),
            info_hash: Some(hash),
            operation_id: op_id,
            subscribe_id: Some(id),
            plugin_id: None,
            subject: "system".into(),
            site_id: None,
            enclosure: enc.clone(),
            ownership_state: store::DownloadOwnershipState::Owned,
            provenance_kind: store::DownloadProvenanceKind::CreatedFresh,
            verified_bytes: Some(1024),
            bytes_digest: None,
            now_utc: 1,
        })
        .unwrap();
    h.qb.inject(owned_task("pausedDL", &enc, &op_tag));
    h.set_search_rows(&[(MATRIX_TITLE, 1)]);
    h.scheduled_search(id).await.unwrap();
    assert_eq!(h.qb.resumes.lock().len(), 1, "paused task resumed");
    assert_eq!(h.pending_state(id, "active"), 1);
}

/// G-M2 follow-up: the manual run path records the broken task as rejected too.
#[tokio::test]
async fn manual_run_rejects_broken_existing_task() {
    let h = Harness::new();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    h.qb.inject(owned_task(
        "missingFiles",
        &h.enclosure(1),
        "cm-op:broken-2",
    ));
    h.set_search_rows(&[(MATRIX_TITLE, 1)]);
    let _ = h.run_subscribe_raw(id).await;
    assert_eq!(h.pending_state(id, "active"), 0, "not an active pending");
    assert_eq!(h.pending_state(id, "rejected"), 1, "manual run rejects it");
}
