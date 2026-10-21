//! Task 13: `PATCH /libraries/{id}` hands a root/locale change to a resumable
//! operation (`202` + `operation_id`), the operation is observable over HTTP until it
//! is terminal, and a Library that is `reindexing` refuses conflicting writes with
//! `409 library.reindexing` while reads keep working.
//!
//! Every assertion reads a public seam: HTTP status + JSON, or committed Store state.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use api::{management::ApiState, router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, Media, MediaId,
    MediaKind, QualitySource, UnitKey,
};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use store::library_write::{FilePublication as StorePublication, WriteScope};
use tower::ServiceExt;

/// The replaced root sits *inside* the new root, mirroring a real "move the Library up
/// one directory" change: every published file keeps a longest-prefix owner in the new
/// set, which is the only way a root can be pruned once nothing references it.
const OLD_ROOT: &str = "media/old-root";
const NEW_ROOT: &str = "media";

struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}

fn request(method: &str, path: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", "Bearer secret")
        .body(Body::empty())
        .unwrap()
}

fn json_request(method: &str, path: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", "Bearer secret")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

fn stage(temp: &tempfile::TempDir) -> ApiState {
    ApiState::new(
        store::Store::open(temp.path().join("data")).unwrap(),
        "secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(temp.path().join("stage"))),
        temp.path().join("library"),
    )
    .unwrap()
}

fn media(store: &store::Store, id: &str) -> Media {
    let media = Media {
        id: id.parse().unwrap(),
        kind: MediaKind::Movie,
        title: "Show".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    media
}

fn root_id_of(store: &store::Store, library_id: &str, path: &Path) -> String {
    store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|root| root.library_id.as_deref() == Some(library_id) && root.path == path)
        .unwrap_or_else(|| panic!("root {path:?} must exist"))
        .id
}

fn publish_one(
    store: &store::Store,
    library_id: domain::LibraryId,
    media: &Media,
    root_id: &str,
    path: &str,
) -> LedgerId {
    let file = LibraryFile {
        file_id: LedgerId::new(),
        library_id,
        root_id: root_id.to_string(),
        media_id: media.id,
        kind: MediaKind::Movie,
        path: PathBuf::from(path),
        version: FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(2048),
        stat_known: true,
        created_ns: None,
        modified_ns: None,
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: Some("1080p".into()),
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: None,
        tracks_version: 0,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    };
    let scope = WriteScope {
        library_id,
        config_revision: 0,
    };
    store
        .publish_files(
            &scope,
            &[StorePublication {
                file: &file,
                units: &[UnitKey::WHOLE],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1_000,
        )
        .unwrap();
    file.file_id
}

struct Seeded {
    library_id: String,
    media_id: MediaId,
    file_id: LedgerId,
    old_root: PathBuf,
}

/// 旧根下已经解析过的 NFO 观测：换根时它必须跟着文件一起改归属，不能留在旧根上，
/// 否则 Library 的元数据输入会和它的 LibraryFile 指向两个不同的根。
/// 文件级观测与 LibraryFile 共用 `path_key`（`roots.rs` 的 re-parent 正是按它迁移）。
fn seed_nfo_input(
    temp: &tempfile::TempDir,
    library_id: &str,
    media_id: MediaId,
    root_id: &str,
    path_key: &str,
) {
    let conn = rusqlite::Connection::open(temp.path().join("data").join("library.db")).unwrap();
    conn.execute(
        "INSERT INTO library_metadata_inputs
            (library_id, media_id, entity_scope, season, episode, locale, input_key,
             root_id, path_key, parse_state, source_version, fields_json,
             present_fields_json, provenance_json, revision)
         VALUES (?1, ?2, 'work', -1, -1, 'zh-CN', 'movie.nfo', ?3, ?4,
                 'valid', 'v1', '{}', '[]', '{}', 1)",
        rusqlite::params![library_id, media_id.to_string(), root_id, path_key],
    )
    .unwrap();
}

fn seed_library(state: &ApiState, temp: &tempfile::TempDir) -> Seeded {
    let old_root = temp.path().join(OLD_ROOT);
    std::fs::create_dir_all(&old_root).unwrap();
    let handle = state.store();
    let store = handle.lock();
    let library = store
        .create_library(
            MediaKind::Movie,
            "Extra",
            &[old_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let root_id = root_id_of(&store, &library.id, &old_root);
    let media = media(&store, "00000000-0000-0000-0000-0000000000c1");
    let file_path = old_root.join("movie.mkv");
    let file_id = publish_one(
        &store,
        library.id.parse().unwrap(),
        &media,
        &root_id,
        file_path.to_str().unwrap(),
    );
    drop(store);
    seed_nfo_input(
        temp,
        &library.id,
        media.id,
        &root_id,
        file_path.to_str().unwrap(),
    );
    Seeded {
        library_id: library.id,
        media_id: media.id,
        file_id,
        old_root,
    }
}

fn new_root(temp: &tempfile::TempDir) -> PathBuf {
    let path = temp.path().join(NEW_ROOT);
    std::fs::create_dir_all(&path).unwrap();
    path
}

async fn poll_operation(
    app: &axum::Router,
    library_id: &str,
    operation_id: &str,
) -> serde_json::Value {
    let path = format!("/api/v1/libraries/{library_id}/operations/{operation_id}");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
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
            assert_ne!(state, "failed", "operation failed: {body}");
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("operation must reach a terminal state")
}

#[tokio::test]
async fn patch_without_roots_stays_synchronous() {
    let temp = tempfile::tempdir().unwrap();
    let state = stage(&temp);
    let seeded = seed_library(&state, &temp);
    let app = router(state.clone());

    let response = app
        .oneshot(json_request(
            "PATCH",
            &format!("/api/v1/libraries/{}", seeded.library_id),
            serde_json::json!({ "name": "Renamed" }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["data"]["name"], "Renamed");
    assert_eq!(
        body["data"]["root_paths"].as_array().unwrap().len(),
        1,
        "a rename must not touch the roots: {body}"
    );
    assert!(
        body["data"].get("operation_id").is_none(),
        "a rename is not an operation: {body}"
    );
    assert_eq!(
        state
            .store()
            .lock()
            .library_lifecycle_state(&seeded.library_id)
            .unwrap(),
        "ready"
    );
}

#[tokio::test]
async fn patch_with_roots_is_accepted_and_migrates_files() {
    let temp = tempfile::tempdir().unwrap();
    let state = stage(&temp);
    let seeded = seed_library(&state, &temp);
    let target = new_root(&temp);
    let app = router(state.clone());

    let response = app
        .clone()
        .oneshot(json_request(
            "PATCH",
            &format!("/api/v1/libraries/{}", seeded.library_id),
            serde_json::json!({ "root_paths": [target.to_str().unwrap()] }),
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::ACCEPTED,
        "a root replacement is a resumable operation"
    );
    let accepted = body_json(response).await;
    let operation_id = accepted["data"]["operation_id"]
        .as_str()
        .unwrap_or_else(|| panic!("202 must carry an operation id: {accepted}"))
        .to_string();
    assert_eq!(accepted["data"]["library_id"], seeded.library_id);
    assert_eq!(accepted["data"]["kind"], "roots");

    let status = poll_operation(&app, &seeded.library_id, &operation_id).await;
    assert_eq!(status["data"]["state"], "succeeded");
    assert_eq!(status["data"]["library_id"], seeded.library_id);

    let handle = state.store();
    let store = handle.lock();
    let library = store.get_library(&seeded.library_id).unwrap().unwrap();
    assert_eq!(library.root_paths, vec![target.clone()]);
    assert_eq!(
        store.library_lifecycle_state(&seeded.library_id).unwrap(),
        "ready",
        "the operation must release the freeze"
    );
    let page = store
        .library_file_page(
            seeded.library_id.parse().unwrap(),
            seeded.media_id,
            None,
            10,
        )
        .unwrap();
    assert_eq!(page.files.len(), 1);
    assert_eq!(page.files[0].file_id, seeded.file_id);
    assert_eq!(
        page.files[0].root_id,
        root_id_of(&store, &seeded.library_id, &target)
    );
    let nfo: (String, String) = {
        let conn = rusqlite::Connection::open(temp.path().join("data").join("library.db")).unwrap();
        conn.query_row(
            "SELECT library_id, root_id FROM library_metadata_inputs WHERE input_key = 'movie.nfo'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(
        nfo.0,
        page.files[0].library_id.to_string(),
        "the NFO observation must stay in the Library that owns its file"
    );
    assert_eq!(
        nfo.1, page.files[0].root_id,
        "the NFO observation must move to the new root together with its file"
    );
}

#[tokio::test]
async fn conflicting_write_is_refused_while_reindexing() {
    let temp = tempfile::tempdir().unwrap();
    let state = stage(&temp);
    let seeded = seed_library(&state, &temp);
    let target = new_root(&temp);
    {
        let handle = state.store();
        let store = handle.lock();
        let revision = store.library_config_revision(&seeded.library_id).unwrap();
        store
            .begin_library_reconfiguration(
                &store::library_operations::LibraryReconfiguration {
                    operation_id: "op-race".into(),
                    library_id: seeded.library_id.parse().unwrap(),
                    expected_revision: revision,
                    root_paths: Some(vec![target.clone()]),
                    display_locale: None,
                },
                1_000,
            )
            .unwrap();
    }
    let app = router(state.clone());

    let response = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{}/scan", seeded.library_id),
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "a scan cannot start while the Library is reindexing"
    );
    let body = body_json(response).await;
    assert_eq!(body["error"]["code"], "library.reindexing");
    assert_eq!(body["error"]["operation_id"], "op-race");

    let response = app
        .oneshot(request(
            "GET",
            &format!("/api/v1/libraries/{}/operations/op-race", seeded.library_id),
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "polling the frozen Library stays available"
    );
    let body = body_json(response).await;
    assert_eq!(body["data"]["operation_id"], "op-race");
}

#[tokio::test]
async fn unknown_or_foreign_operation_is_not_found() {
    let temp = tempfile::tempdir().unwrap();
    let state = stage(&temp);
    let seeded = seed_library(&state, &temp);
    let target = new_root(&temp);
    {
        let handle = state.store();
        let store = handle.lock();
        let revision = store.library_config_revision(&seeded.library_id).unwrap();
        store
            .begin_library_reconfiguration(
                &store::library_operations::LibraryReconfiguration {
                    operation_id: "op-owned".into(),
                    library_id: seeded.library_id.parse().unwrap(),
                    expected_revision: revision,
                    root_paths: Some(vec![target.clone()]),
                    display_locale: None,
                },
                1_000,
            )
            .unwrap();
        let other_root = temp.path().join("other-root");
        std::fs::create_dir_all(&other_root).unwrap();
        store
            .create_library(
                MediaKind::Movie,
                "Other",
                &[other_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
    }
    let other = state
        .store()
        .lock()
        .list_libraries()
        .unwrap()
        .into_iter()
        .find(|library| library.name == "Other")
        .unwrap();
    let app = router(state.clone());

    for (library_id, operation_id) in [
        (seeded.library_id.clone(), "op-missing".to_string()),
        (other.id.clone(), "op-owned".to_string()),
    ] {
        let response = app
            .clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{library_id}/operations/{operation_id}"),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = body_json(response).await;
        assert_eq!(body["error"]["code"], "library.operation_not_found");
    }

    let response = app
        .oneshot(request(
            "GET",
            &format!(
                "/api/v1/libraries/{}/operations/op-owned",
                seeded.library_id
            ),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["data"]["operation_id"], "op-owned");
    assert_eq!(body["data"]["kind"], "roots");
}

#[tokio::test]
async fn reindexing_refuses_library_writes_without_half_applying_them() {
    let temp = tempfile::tempdir().unwrap();
    let state = stage(&temp);
    let seeded = seed_library(&state, &temp);
    let target = new_root(&temp);
    {
        let handle = state.store();
        let store = handle.lock();
        let revision = store.library_config_revision(&seeded.library_id).unwrap();
        store
            .begin_library_reconfiguration(
                &store::library_operations::LibraryReconfiguration {
                    operation_id: "op-race".into(),
                    library_id: seeded.library_id.parse().unwrap(),
                    expected_revision: revision,
                    root_paths: Some(vec![target.clone()]),
                    display_locale: None,
                },
                1_000,
            )
            .unwrap();
    }
    let app = router(state.clone());

    let response = app
        .oneshot(json_request(
            "PATCH",
            &format!("/api/v1/libraries/{}", seeded.library_id),
            serde_json::json!({ "name": "Renamed", "root_paths": [target.to_str().unwrap()] }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = body_json(response).await;
    assert_eq!(body["error"]["code"], "library.reindexing");
    assert_eq!(body["error"]["operation_id"], "op-race");

    let library = state
        .store()
        .lock()
        .get_library(&seeded.library_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        library.name, "Extra",
        "the refused request must not half-apply its rename"
    );
    assert_eq!(library.root_paths, vec![seeded.old_root.clone()]);
}

#[tokio::test]
async fn startup_recovery_advances_unfinished_operation_once() {
    let temp = tempfile::tempdir().unwrap();
    let state = stage(&temp);
    let seeded = seed_library(&state, &temp);
    let target = new_root(&temp);
    // A previous process froze the Library and died before applying the new roots; no
    // driver is running for it in this process.
    {
        let handle = state.store();
        let store = handle.lock();
        let revision = store.library_config_revision(&seeded.library_id).unwrap();
        store
            .begin_library_reconfiguration(
                &store::library_operations::LibraryReconfiguration {
                    operation_id: "op-crash".into(),
                    library_id: seeded.library_id.parse().unwrap(),
                    expected_revision: revision,
                    root_paths: Some(vec![target.clone()]),
                    display_locale: None,
                },
                1_000,
            )
            .unwrap();
        assert_eq!(
            store.library_lifecycle_state(&seeded.library_id).unwrap(),
            "reindexing"
        );
    }
    let app = router(state.clone());
    let job_loop = api::job_loop::spawn_job_loop_with(
        state.clone(),
        std::time::Duration::from_secs(3_600),
        api::job_loop::unix_now,
    );

    let status = poll_operation(&app, &seeded.library_id, "op-crash").await;
    assert_eq!(status["data"]["state"], "succeeded");
    job_loop.abort();

    let settled = {
        let handle = state.store();
        let store = handle.lock();
        assert_eq!(
            store.library_lifecycle_state(&seeded.library_id).unwrap(),
            "ready",
            "recovery must release the freeze"
        );
        let page = store
            .library_file_page(
                seeded.library_id.parse().unwrap(),
                seeded.media_id,
                None,
                10,
            )
            .unwrap();
        assert_eq!(
            page.files[0].root_id,
            root_id_of(&store, &seeded.library_id, &target),
            "recovery must finish the migration the previous process started"
        );
        store
            .library_operation_view("op-crash")
            .unwrap()
            .expect("the operation row must stay observable after recovery")
    };
    assert_eq!(settled.state, "succeeded");

    // A terminal operation is never replayed: a second recovery changes no committed fact.
    api::storage_operations::resume_library_reconfigurations(&state).unwrap();
    let again = state
        .store()
        .lock()
        .library_operation_view("op-crash")
        .unwrap()
        .unwrap();
    assert_eq!(again.state, settled.state);
    assert_eq!(
        again.updated_at_ms, settled.updated_at_ms,
        "a completed operation must not be re-executed"
    );
    assert_eq!(again.cursor, settled.cursor);
}

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
            let phase = body["data"]["state"].as_str().unwrap_or_default().to_string();
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
