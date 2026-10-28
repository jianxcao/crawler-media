//! Library entity CRUD: create / rename / re-root / delete / default / reorder.

use axum::Json;
use axum::extract::{Path, State, rejection::JsonRejection};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::MediaKind;
use serde_json::{Value, json};
use std::str::FromStr;

use crate::http::{err, ok};
use crate::management::ApiState;
use crate::storage_operations::StorageOperationError;
use crate::store::StoreError;

use super::library::missing_library;

pub(crate) mod create_input;
mod patch_input;

fn parse_member_ids(body: &Value) -> Vec<domain::UserId> {
    body["member_ids"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().and_then(|s| domain::UserId::from_str(s).ok()))
                .collect()
        })
        .unwrap_or_default()
}

fn paths_from(body: &Value) -> Vec<String> {
    body["root_paths"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// POST /libraries — create a library with one or more ordered roots
/// (first = primary). Unknown or retired fields are rejected before writing.
pub(crate) async fn create_library(
    State(state): State<ApiState>,
    body: Result<Json<create_input::CreateLibrary>, JsonRejection>,
) -> Response {
    let body = match body {
        Ok(Json(body)) => match serde_json::to_value(body) {
            Ok(body) => body,
            Err(error) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "library.serialize",
                    &error.to_string(),
                );
            }
        },
        Err(error) => {
            return err(
                StatusCode::BAD_REQUEST,
                "library.invalid",
                &error.body_text(),
            );
        }
    };
    let kind = match MediaKind::from_str(body["kind"].as_str().unwrap_or_default()) {
        Ok(kind) => kind,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "library.invalid_kind",
                "kind 必须是 movie / tv / video",
            );
        }
    };
    let name = body["name"].as_str().unwrap_or_default().trim().to_string();
    if name.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "library.invalid_name",
            "库名称不能为空",
        );
    }
    let root_paths = paths_from(&body);
    if root_paths.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "library.invalid_roots",
            "至少需要一个目录",
        );
    }
    let work_state = state.clone();
    let created = state
        .fs_work
        .run_to_completion(move || {
            let refs: Vec<&str> = root_paths.iter().map(String::as_str).collect();
            let roots = store::PreparedLibraryRoots::observe(&refs)?;
            create_library_record(&work_state, &body, kind, &name, &roots)
        })
        .await;
    let library = match created {
        Ok(Ok(library)) => library,
        Ok(Err(error)) => {
            return err(
                StatusCode::BAD_REQUEST,
                "library.invalid_roots",
                &error.to_string(),
            );
        }
        Err(error) => return super::library::browse_work_error(error),
    };
    crate::root_reassign_work::notify(&state);
    state.scan_coordinator.trigger_next(&state);
    render_library(&state, &library).await
}

fn create_library_record(
    state: &ApiState,
    body: &Value,
    kind: MediaKind,
    name: &str,
    roots: &store::PreparedLibraryRoots,
) -> Result<crate::store::Library, StoreError> {
    let members = parse_member_ids(body);
    let rules = body["match_rules"].as_array().cloned().unwrap_or_default();
    let filter = body
        .get("default_filter_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let store = state.store.lock();
    let library = store.create_library_full_prepared(
        kind,
        name,
        roots,
        body["access_mode"].as_str().unwrap_or("everyone"),
        body["admin_visible"].as_bool().unwrap_or(true),
        &members,
        &rules,
        filter,
    )?;
    store.set_library_intro_settings(
        &library.id,
        body["detect_intros"].as_bool().unwrap_or(false),
        body["enable_fingerprint"].as_bool().unwrap_or(false),
    )?;
    store.set_library_switch_settings(
        &library.id,
        body["realtime_watch"].as_bool(),
        body["generate_thumbnails"].as_bool(),
        body["extract_chapter_images"].as_bool(),
        body["exclude_from_home"].as_bool(),
        body["auto_series_collections"].as_bool(),
    )?;
    let library = store
        .get_library(&library.id)?
        .ok_or_else(|| StoreError::Missing(library.id))?;
    let signature = crate::scan_operations::scan_scope_signature(&library);
    if let Err(error) = store.enqueue_scan(
        &library.id,
        &signature,
        &[],
        crate::job_loop::unix_now() * 1000,
    ) {
        tracing::error!(library_id = %library.id, %error, "初始扫描入队失败");
    }
    Ok(library)
}

async fn render_library(state: &ApiState, library: &crate::store::Library) -> Response {
    let library_snapshot = library.clone();
    let loaded = super::store_wait::store_read(state, move |store| {
        let library = &library_snapshot;
        let stats = match crate::library_statistics::statistics_for(&store, library) {
            Ok(stats) => stats,
            Err(error) => {
                return Err(err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                ));
            }
        };
        Ok((super::library::library_facts(store, library), stats))
    })
    .await;
    let (facts, stats) = match loaded {
        Ok(Ok(value)) => value,
        Ok(Err(response)) => return response,
        Err(error) => return error.response(),
    };
    match state
        .http_work
        .run(super::library::BROWSE_DEADLINE, move || {
            super::library::library_json_from_facts(&facts, Some(stats))
        })
        .await
    {
        Ok(value) => ok(value).into_response(),
        Err(error) => super::library::browse_work_error(error),
    }
}

/// PATCH /libraries/{id} — rename, access, switches, and root/locale migration.
///
/// Plain fields stay synchronous. A `root_paths` or `display_locale` change rewrites
/// `library_files` and NFO ownership in batches, so it answers `202` with an operation
/// the caller polls at `GET /libraries/{id}/operations/{operation_id}`.
pub(crate) async fn patch_library(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Err(error) = serde_json::from_value::<patch_input::PatchLibrary>(body.clone()) {
        return err(
            StatusCode::BAD_REQUEST,
            "library.invalid",
            &error.to_string(),
        );
    }
    let root_paths = body["root_paths"].is_array().then(|| paths_from(&body));
    if let Some(paths) = &root_paths
        && paths.is_empty()
    {
        return err(
            StatusCode::BAD_REQUEST,
            "library.invalid_roots",
            "媒体库至少需要一个目录",
        );
    }
    let display_locale = body["display_locale"]
        .as_str()
        .map(str::trim)
        .filter(|locale| !locale.is_empty())
        .map(str::to_string);
    // 非批量字段先同步落库：失败时不启动任何操作，语义与旧版一致。
    let settings_state = state.clone();
    let settings_id = id.clone();
    let settings_body = body.clone();
    let library = match state
        .http_work
        .run_to_completion(move || {
            apply_library_settings(&settings_state, &settings_id, &settings_body)
        })
        .await
    {
        Ok(Ok(library)) => library,
        Ok(Err(response)) => return response,
        Err(error) => return super::library::browse_work_error(error),
    };
    let request = crate::storage_operations::ReconfigurationRequest {
        root_paths,
        display_locale,
    };
    if request.root_paths.is_none() && request.display_locale.is_none() {
        return render_library(&state, &library).await;
    }
    // 竞态窗口：上面的字段已经落库，而 begin 仍可能被并发操作拒绝；此时 409 里带上
    // 对方的 operation_id，前端轮询它就能看到最终结果。
    let work_state = state.clone();
    let begun = state
        .fs_work
        .run_to_completion(move || {
            crate::storage_operations::begin_library_reconfiguration(&work_state, &id, request)
        })
        .await;
    let begun = match begun {
        Ok(result) => result,
        Err(error) => return super::library::browse_work_error(error),
    };
    match begun {
        Ok(operation) => (StatusCode::ACCEPTED, ok(operation)).into_response(),
        Err(StorageOperationError::Busy) => super::store_wait::busy_response(),
        Err(StorageOperationError::MissingLibrary) => missing_library(),
        Err(StorageOperationError::Invalid(message)) => err(
            StatusCode::BAD_REQUEST,
            "library.invalid_operation",
            &message,
        ),
        Err(StorageOperationError::Reindexing { operation_id }) => {
            super::library_operations::reindexing_conflict(operation_id.as_deref())
        }
        Err(StorageOperationError::Store(StoreError::Missing(_))) => missing_library(),
        Err(StorageOperationError::Store(StoreError::Invalid(message))) => err(
            StatusCode::BAD_REQUEST,
            "library.invalid_operation",
            &message,
        ),
        Err(StorageOperationError::Store(error)) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}

/// Apply the synchronous fields of a PATCH and return the updated Library.
///
/// Roots are deliberately absent: they belong to the batch reconfiguration, never to a
/// single `update_library_full` call.
fn apply_library_settings(
    state: &ApiState,
    id: &str,
    body: &Value,
) -> Result<crate::store::Library, Response> {
    let name = body["name"].as_str().map(str::to_string);
    let store = state.store.lock();
    let Some(current) = (match store.get_library(id) {
        Ok(library) => library,
        Err(error) => {
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            ));
        }
    }) else {
        return Err(missing_library());
    };
    apply_library_toggles(&store, id, body, &current)?;
    let access = ["access_mode", "admin_visible", "member_ids"]
        .iter()
        .any(|field| body.get(*field).is_some())
        .then(|| {
            (
                body["access_mode"].as_str().unwrap_or(&current.access_mode),
                body["admin_visible"]
                    .as_bool()
                    .unwrap_or(current.admin_visible),
                if body.get("member_ids").is_some() {
                    parse_member_ids(body)
                } else {
                    current.member_ids.clone()
                },
            )
        });
    let match_rules = body["match_rules"].as_array().cloned();
    let default_filter_id_update: Option<Option<&str>> = if body.get("default_filter_id").is_some()
    {
        Some(body["default_filter_id"].as_str().filter(|s| !s.is_empty()))
    } else {
        None
    };
    match store.update_library_full(
        id,
        name.as_deref(),
        None,
        access
            .as_ref()
            .map(|(mode, visible, ids)| (*mode, *visible, ids.as_slice())),
        match_rules.as_deref(),
        default_filter_id_update,
    ) {
        Ok(library) => Ok(library),
        Err(StoreError::Missing(_)) => Err(missing_library()),
        Err(StoreError::Protected(message)) => {
            Err(err(StatusCode::BAD_REQUEST, "library.protected", &message))
        }
        Err(error) => Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        )),
    }
}

/// Apply the two independent toggle groups a PATCH may carry.
fn apply_library_toggles(
    store: &crate::Store,
    id: &str,
    body: &Value,
    current: &crate::store::Library,
) -> Result<(), Response> {
    if body.get("detect_intros").is_some() || body.get("enable_fingerprint").is_some() {
        let detect = body["detect_intros"]
            .as_bool()
            .unwrap_or(current.detect_intros);
        let enable_fp = body["enable_fingerprint"]
            .as_bool()
            .unwrap_or(current.enable_fingerprint);
        if let Err(error) = store.set_library_intro_settings(id, detect, enable_fp) {
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            ));
        }
    }
    let switch_fields = [
        "realtime_watch",
        "generate_thumbnails",
        "extract_chapter_images",
        "exclude_from_home",
        "auto_series_collections",
    ];
    if switch_fields.iter().any(|field| body.get(*field).is_some()) {
        let rw = body.get("realtime_watch").and_then(|v| v.as_bool());
        let gt = body.get("generate_thumbnails").and_then(|v| v.as_bool());
        let ec = body.get("extract_chapter_images").and_then(|v| v.as_bool());
        let efh = body.get("exclude_from_home").and_then(|v| v.as_bool());
        let asc = body
            .get("auto_series_collections")
            .and_then(|v| v.as_bool());
        if let Err(error) = store.set_library_switch_settings(id, rw, gt, ec, efh, asc) {
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            ));
        }
    }
    Ok(())
}

/// DELETE /libraries/{id} — remove the library and its roots. The last library
/// of a kind is protected.
pub(crate) async fn delete_library(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let preflight = state.library_work.freeze(&state.store, &id);
    match preflight {
        Ok(operation) => match crate::library_lifecycle::launch_library_removal(
            &state,
            operation,
            tokio::runtime::Handle::current(),
        ) {
            Ok(operation) => (StatusCode::ACCEPTED, ok(json!(operation))).into_response(),
            Err(error) => err(StatusCode::INTERNAL_SERVER_ERROR, "library.removal", &error),
        },
        Err(StoreError::Missing(_)) => missing_library(),
        Err(StoreError::Protected(message)) => {
            err(StatusCode::CONFLICT, "library.protected", &message)
        }
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}

/// PUT /libraries/{id}/default — promote to the kind's default library.
pub(crate) async fn set_default_library(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let result =
        match super::store_wait::store_write(&state, move |store| store.set_default_library(&id))
            .await
        {
            Ok(result) => result,
            Err(response) => return response,
        };
    match result {
        Ok(library) => render_library(&state, &library).await,
        Err(StoreError::Missing(_)) => missing_library(),
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}

/// PUT /libraries/order — full display order of all libraries (`ids`).
pub(crate) async fn reorder_libraries(
    State(state): State<ApiState>,
    Json(body): Json<Value>,
) -> Response {
    let ids: Vec<String> = body["ids"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    super::store_wait::store_write(&state, move |store| {
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        match store.reorder_libraries(&refs) {
            Ok(()) => ok(json!({ "reordered": refs.len() })).into_response(),
            Err(error) => err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            ),
        }
    })
    .await
    .unwrap_or_else(|response| response)
}

/// GET /libraries/routing-options — return TMDB genres, country region presets, and country names.
pub(crate) async fn routing_options(State(state): State<ApiState>) -> Response {
    let worker = state.clone();
    match state
        .http_work
        .run(super::library::BROWSE_DEADLINE, move || {
            routing_options_blocking(&worker)
        })
        .await
    {
        Ok(response) => response,
        Err(error) => super::library::browse_work_error(error),
    }
}

fn routing_options_blocking(state: &ApiState) -> Response {
    let catalog = state.catalog.as_ref();
    let movie_genres: Vec<Value> = catalog
        .genres(MediaKind::Movie)
        .unwrap_or_default()
        .into_iter()
        .map(|(id, name)| json!({ "id": id, "label": name }))
        .collect();
    let tv_genres: Vec<Value> = catalog
        .genres(MediaKind::Tv)
        .unwrap_or_default()
        .into_iter()
        .map(|(id, name)| json!({ "id": id, "label": name }))
        .collect();

    let region_presets = json!([
        {
            "key": "chinese",
            "label": "华语（大陆/港台）",
            "countries": ["CN", "HK", "TW"]
        },
        {
            "key": "western",
            "label": "欧美",
            "countries": ["US", "GB", "FR", "DE", "CA", "AU", "IT", "ES"]
        },
        {
            "key": "east_asia",
            "label": "日韩",
            "countries": ["JP", "KR"]
        },
        {
            "key": "southeast_asia",
            "label": "东南亚",
            "countries": ["TH", "SG", "MY", "VN", "ID", "PH"]
        }
    ]);

    let country_names = json!({
        "CN": "中国大陆",
        "HK": "中国香港",
        "TW": "中国台湾",
        "JP": "日本",
        "KR": "韩国",
        "US": "美国",
        "GB": "英国",
        "FR": "法国",
        "DE": "德国",
        "CA": "加拿大",
        "AU": "澳大利亚",
        "IT": "意大利",
        "ES": "西班牙",
        "TH": "泰国",
        "SG": "新加坡",
        "MY": "马来西亚",
        "IN": "印度",
        "RU": "俄罗斯"
    });

    ok(json!({
        "movie_genres": movie_genres,
        "tv_genres": tv_genres,
        "region_presets": region_presets,
        "country_names": country_names
    }))
    .into_response()
}
