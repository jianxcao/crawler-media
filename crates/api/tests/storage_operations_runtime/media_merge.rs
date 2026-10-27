//! Media-merge runtime assertions, split out of `storage_operations_runtime.rs`
//! to keep the suite under the 800-line file limit.

use super::*;

/// 合并请求的两条断言共用一对同类型 Media（`media()` 不带外部 id，因此没有
/// 需要人工裁决的 refresh target）。
fn sample_media_ids() -> (&'static str, &'static str) {
    (
        "00000000-0000-0000-0000-0000000000a1",
        "00000000-0000-0000-0000-0000000000a2",
    )
}

const MERGE_REQUEST_ID: &str = "00000000-0000-0000-0000-00000000a101";

fn merge_body(request_id: &str, survivor: &str, loser: &str) -> serde_json::Value {
    serde_json::json!({
        "request_id": request_id,
        "survivor": survivor,
        "loser": loser,
        "survivor_revision": 0,
        "loser_revision": 0,
    })
}

async fn post_merge(
    app: &axum::Router,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(json_request("POST", "/api/v1/media/merges", body))
        .await
        .unwrap();
    let status = response.status();
    (status, body_json(response).await)
}

/// 身份状态没有公开读接口（只有 HTTP 状态与已提交行有意义），直接读已提交的
/// `media` 行，与 store 侧测试的读法一致。
fn identity_state(data_dir: &Path, media_id: &str) -> (String, Option<String>) {
    let db = rusqlite::Connection::open_with_flags(
        data_dir.join("library.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    db.query_row(
        "SELECT identity_state, survivor_id FROM media WHERE media_id = ?1",
        rusqlite::params![media_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .unwrap()
}

#[tokio::test]
async fn media_merge_is_idempotent_and_rejects_a_different_plan() {
    let temp = tempfile::tempdir().unwrap();
    let state = stage(&temp);
    let (survivor, loser) = sample_media_ids();
    {
        let handle = state.store();
        let store = handle.lock();
        media(&store, survivor);
        media(&store, loser);
    }
    let app = router(state.clone());
    let body = merge_body(MERGE_REQUEST_ID, survivor, loser);

    let (status, accepted) = post_merge(&app, body.clone()).await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "a merge is accepted, not executed inline: {accepted}"
    );
    assert_eq!(
        accepted["data"]["operation_id"], MERGE_REQUEST_ID,
        "the request id is the operation id, so a replay addresses the same row"
    );

    let (replay_status, replayed) = post_merge(&app, body).await;
    assert_eq!(replay_status, StatusCode::ACCEPTED);
    assert_eq!(replayed["data"]["operation_id"], MERGE_REQUEST_ID);

    let mut different_plan = merge_body(MERGE_REQUEST_ID, survivor, loser);
    different_plan["survivor_revision"] = serde_json::json!(1);
    let (conflict_status, conflict) = post_merge(&app, different_plan).await;
    assert_eq!(
        conflict_status,
        StatusCode::CONFLICT,
        "one operation id cannot carry two plans: {conflict}"
    );
    assert_eq!(conflict["error"]["code"], "media.merge_conflict");

    let missing = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/media/operations/00000000-0000-0000-0000-00000000dead",
        ))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        body_json(missing).await["error"]["code"],
        "media.operation_missing"
    );

    let (invalid_status, invalid) =
        post_merge(&app, merge_body("not-a-uuid", survivor, loser)).await;
    assert_eq!(
        invalid_status,
        StatusCode::BAD_REQUEST,
        "a non-UUID request id cannot address a persisted operation: {invalid}"
    );
    assert_eq!(invalid["error"]["code"], "media.merge_invalid");
}

#[tokio::test]
async fn media_merge_driver_retires_the_loser() {
    let temp = tempfile::tempdir().unwrap();
    let state = stage(&temp);
    let (survivor, loser) = sample_media_ids();
    {
        let handle = state.store();
        let store = handle.lock();
        media(&store, survivor);
        media(&store, loser);
    }
    let app = router(state.clone());
    let data_dir = temp.path().join("data");
    // 这两个 Media 没有外部 id，插入时是 `provisional`；合并冻结它们再放行时，存活方
    // 必须回到冻结前的身份，而不是被合并「顺手确认」成别的状态。
    let survivor_before = identity_state(&data_dir, survivor);
    let (status, accepted) = post_merge(&app, merge_body(MERGE_REQUEST_ID, survivor, loser)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");

    let job_loop = api::job_loop::spawn_job_loop_with(
        state.clone(),
        std::time::Duration::from_millis(1),
        api::job_loop::unix_now,
    );
    let path = format!("/api/v1/media/operations/{MERGE_REQUEST_ID}");
    let settled = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let response = app.clone().oneshot(request("GET", &path)).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = body_json(response).await;
            let state = body["data"]["state"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if state == "succeeded" {
                break body;
            }
            assert_ne!(state, "failed", "merge failed: {body}");
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the background driver must advance the merge to a terminal state");
    assert_eq!(settled["data"]["operation_id"], MERGE_REQUEST_ID);
    job_loop.abort();

    assert_eq!(
        identity_state(&data_dir, loser),
        ("retired".to_string(), Some(survivor.to_string())),
        "the loser must be retired into the survivor"
    );
    assert_eq!(
        identity_state(&data_dir, survivor),
        survivor_before,
        "the survivor must be released back to the identity it was frozen with"
    );
    assert_eq!(
        state.store().lock().unfinished_media_merges().unwrap(),
        Vec::<String>::new(),
        "a succeeded merge must not stay in the recovery set"
    );
}

/// 建一条 `catalog_refresh` 定时定义：payload 形状与 `subscribe_create::seed_jobs`
/// 写入的一致，但 `enabled = false` —— 本测试只关心它的 payload 被收敛，不要它真的跑。
fn seed_catalog_refresh_def(state: &ApiState, media_id: &str) -> domain::JobDefId {
    let payload = serde_json::json!({ "media_id": media_id }).to_string();
    let def = state
        .jobs()
        .lock()
        .upsert_def(jobs::NewDef {
            kind: jobs::JobKind::CatalogRefresh,
            name: format!("catalog {media_id}"),
            enabled: false,
            schedule: None,
            payload,
            timeout_secs: Some(120),
            concurrency_key: Some(format!("catalog:{media_id}")),
        })
        .unwrap();
    def.id
}

fn payload_media_id(state: &ApiState, def_id: domain::JobDefId) -> String {
    let def = state
        .jobs()
        .lock()
        .def(def_id)
        .unwrap()
        .expect("the seeded definition must still exist");
    let payload: serde_json::Value = serde_json::from_str(&def.payload).unwrap();
    payload["media_id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn media_merge_converges_queued_job_references_once() {
    let temp = tempfile::tempdir().unwrap();
    let state = stage(&temp);
    let (survivor, loser) = sample_media_ids();
    {
        let handle = state.store();
        let store = handle.lock();
        media(&store, survivor);
        media(&store, loser);
    }
    let def_id = seed_catalog_refresh_def(&state, loser);
    assert_eq!(
        payload_media_id(&state, def_id),
        loser,
        "the fixture must start out pointing at the loser"
    );

    let app = router(state.clone());
    let (status, accepted) = post_merge(&app, merge_body(MERGE_REQUEST_ID, survivor, loser)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");

    let job_loop = api::job_loop::spawn_job_loop_with(
        state.clone(),
        std::time::Duration::from_millis(1),
        api::job_loop::unix_now,
    );
    let path = format!("/api/v1/media/operations/{MERGE_REQUEST_ID}");
    let settle = async {
        loop {
            let response = app.clone().oneshot(request("GET", &path)).await.unwrap();
            let body = body_json(response).await;
            let phase = body["data"]["state"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if phase == "succeeded" {
                return;
            }
            assert_ne!(phase, "failed", "merge failed: {body}");
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), settle)
        .await
        .expect("the driver must advance the merge to a terminal state");

    let converge = async {
        while payload_media_id(&state, def_id) != survivor {
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), converge)
        .await
        .expect("a queued catalog refresh must be rewritten onto the survivor");
    job_loop.abort();

    assert_eq!(
        api::storage_operations::converge_retired_media_refs(&state),
        0,
        "references already point at the survivor, so a second round rewrites nothing"
    );
    assert_eq!(
        payload_media_id(&state, def_id),
        survivor,
        "convergence must stay idempotent"
    );
}
