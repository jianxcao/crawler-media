use std::collections::HashSet;

use domain::UserId;
use serde::Serialize;
use sha2::{Digest, Sha256};
use store::Db;
use store::download_operations::{BoostProtectionState, BoostTaskFactRecord, BoostTaskState};

use super::snapshot::{
    expand_path_set_off_lock, expand_protection_sets_off_lock, extract_plan_paths, path_is_in_set,
};
use crate::download_service::plugin_command_ops::{
    collect_db_sibling_paths, extend_sibling_paths_from_live_downloaders,
    is_active_boost_task_state,
};
use crate::http::store_wait::store_read;
use crate::job_loop::unix_now;
use crate::management::ApiState;
use crate::plugins::PluginServiceError;
use crate::plugins::reauth::CleanupTokenBinding;

#[derive(Clone, Debug)]
pub(crate) enum CleanupPreviewQuery {
    Selection { task_ids: Vec<String> },
    Plan { plan_handle: String },
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CleanupPreviewTaskDto {
    pub task_id: String,
    pub downloader_id: String,
    pub info_hash: String,
    pub state: String,
    pub retained_bytes: u64,
    pub paths: Vec<String>,
    pub eligible: bool,
    pub blocked_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct BrushCleanupPreviewDto {
    pub mode: &'static str,
    pub plan_handle: Option<String>,
    pub revision: Option<i64>,
    pub confirmation_token: Option<String>,
    pub expires_at: Option<String>,
    pub plan_digest: Option<String>,
    pub total_retained_bytes: u64,
    pub eligible_count: usize,
    pub blocked_count: usize,
    pub tasks: Vec<CleanupPreviewTaskDto>,
}

pub(crate) fn compute_plan_digest(raw_json: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw_json.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

fn evaluate_task_cleanup_block(
    tf: &BoostTaskFactRecord,
    paths: &[String],
    protections: &store::download_operations::BoostProtectionSets,
    sibling_paths: &HashSet<String>,
    now: i64,
) -> Option<String> {
    if tf.hr_active != Some(false) {
        return Some("hr-active-or-unknown".into());
    }
    if tf.hr_until.is_some_and(|u| u > now) {
        return Some("hr-until-active".into());
    }
    if tf.hold_until.is_some_and(|u| u > now) {
        return Some("hold-until-active".into());
    }
    if tf.protection_state != BoostProtectionState::Eligible {
        return Some(format!("protection:{}", tf.protection_state.as_str()));
    }
    let hash_norm = tf.info_hash.trim().to_ascii_lowercase();
    if protections.active_subscribe_hashes.contains(&hash_norm) {
        return Some("subscribe-active".into());
    }
    if paths
        .iter()
        .any(|p| path_is_in_set(p, &protections.library_paths))
    {
        return Some("library-referenced".into());
    }
    if paths
        .iter()
        .any(|p| path_is_in_set(p, &protections.transfer_paths))
    {
        return Some("transfer-active".into());
    }
    if paths.iter().any(|p| path_is_in_set(p, sibling_paths)) {
        return Some("shared-with-active-task".into());
    }
    if matches!(
        tf.state,
        BoostTaskState::RetainedOnly | BoostTaskState::CleanupPlanned
    ) && (paths.is_empty() || paths.iter().any(|p| p.trim().is_empty()))
    {
        return Some("manual-cleanup-required".into());
    }
    None
}

fn sibling_active_paths(
    state: &ApiState,
    selected_tasks: &[BoostTaskFactRecord],
    all_tasks: &[BoostTaskFactRecord],
    exclude_ids: &HashSet<String>,
) -> Result<HashSet<String>, PluginServiceError> {
    let (mut set, db_active_hashes) = collect_db_sibling_paths(all_tasks, exclude_ids);
    let mut dl_ids = Vec::new();
    let mut excluded_hashes = HashSet::new();
    for tf in selected_tasks {
        if !dl_ids.contains(&tf.downloader_id) {
            dl_ids.push(tf.downloader_id);
        }
        if is_active_boost_task_state(tf.state) {
            let h = tf.info_hash.trim().to_ascii_lowercase();
            if !h.is_empty() {
                excluded_hashes.insert((tf.downloader_id, h));
            }
        }
    }
    for (sib_dl, _) in &db_active_hashes {
        if !dl_ids.contains(sib_dl) {
            dl_ids.push(*sib_dl);
        }
    }
    let all_dls = state.store.list_downloaders()?;
    for row in &all_dls {
        if row.enabled && !dl_ids.contains(&row.id) {
            dl_ids.push(row.id);
        }
    }
    if !all_dls.iter().any(|r| r.enabled && r.is_default) {
        let nil_id = domain::DownloaderId::from_uuid(uuid::Uuid::nil());
        if !dl_ids.contains(&nil_id) {
            dl_ids.push(nil_id);
        }
    }
    extend_sibling_paths_from_live_downloaders(
        state,
        &dl_ids,
        &excluded_hashes,
        &db_active_hashes,
        &mut set,
    )
    .map_err(|e| PluginServiceError::Io(format!("live downloader sibling probe failed: {e}")))?;
    expand_path_set_off_lock(&mut set);
    Ok(set)
}

async fn load_preview_context(
    state: &ApiState,
    pid: String,
    query: CleanupPreviewQuery,
    now: i64,
) -> Result<
    (
        store::plugins::PluginRecord,
        Vec<BoostTaskFactRecord>,
        Vec<BoostTaskFactRecord>,
        store::download_operations::BoostProtectionSets,
        bool,
    ),
    PluginServiceError,
> {
    let is_plan = matches!(query, CleanupPreviewQuery::Plan { .. });
    let (plugin, selected_tasks, all_tasks, protections) = store_read(
        state,
        &[Db::Library, Db::App, Db::Subscribe],
        move |store| {
            let plugin = store.get_plugin(&pid)?.ok_or_else(|| {
                PluginServiceError::not_found("plugin.missing", "plugin not found")
            })?;
            let protections = store.load_boost_protection_sets()?;
            let all_tasks = store.list_active_boost_task_facts()?;
            let selected = match &query {
                CleanupPreviewQuery::Selection { task_ids } => {
                    let mut found = Vec::new();
                    for tid in task_ids {
                        let tf = store
                            .find_boost_task_fact_by_handle(&pid, tid)?
                            .ok_or_else(|| {
                                PluginServiceError::not_found(
                                    "plugin.task_missing",
                                    format!("boost task `{tid}` not found"),
                                )
                            })?;
                        found.push(tf);
                    }
                    found
                }
                CleanupPreviewQuery::Plan { plan_handle } => {
                    let tf = store
                        .get_boost_task_fact_by_plan(plan_handle)?
                        .filter(|tf| tf.plugin_id == pid)
                        .ok_or_else(|| {
                            PluginServiceError::not_found(
                                "plugin.cleanup_plan_missing",
                                "cleanup plan not found",
                            )
                        })?;
                    if tf.state != BoostTaskState::CleanupPlanned
                        || tf.cleanup_plan_expires_at.is_none_or(|e| e < now)
                    {
                        return Err(PluginServiceError::conflict(
                            "plugin.cleanup_plan_expired",
                            "cleanup plan is not active or has expired",
                        ));
                    }
                    vec![tf]
                }
            };
            Ok((plugin, selected, all_tasks, protections))
        },
    )
    .await
    .map_err(super::super::service::map_store_call_error)??;
    Ok((plugin, selected_tasks, all_tasks, protections, is_plan))
}

fn build_preview_tasks(
    state: &ApiState,
    selected_tasks: &[BoostTaskFactRecord],
    all_tasks: &[BoostTaskFactRecord],
    protections: &store::download_operations::BoostProtectionSets,
    now: i64,
) -> Result<(Vec<CleanupPreviewTaskDto>, u64, usize, usize), PluginServiceError> {
    let selected_ids: HashSet<String> = selected_tasks
        .iter()
        .map(|t| t.task_fact_id.clone())
        .collect();
    let sibling_paths = sibling_active_paths(state, selected_tasks, all_tasks, &selected_ids)?;

    let mut tasks_dto = Vec::with_capacity(selected_tasks.len());
    let mut total_retained_bytes = 0u64;
    let mut eligible_count = 0usize;
    let mut blocked_count = 0usize;
    for tf in selected_tasks {
        let paths = extract_plan_paths(tf);
        let blocked = evaluate_task_cleanup_block(tf, &paths, protections, &sibling_paths, now);
        let eligible = blocked.is_none();
        if eligible {
            eligible_count += 1;
        } else {
            blocked_count += 1;
        }
        let retained = u64::try_from(tf.retained_bytes).unwrap_or(0);
        total_retained_bytes = total_retained_bytes.saturating_add(retained);
        tasks_dto.push(CleanupPreviewTaskDto {
            task_id: tf.task_fact_id.clone(),
            downloader_id: tf.downloader_id.to_string(),
            info_hash: tf.info_hash.clone(),
            state: tf.state.as_str().to_string(),
            retained_bytes: retained,
            paths,
            eligible,
            blocked_reason: blocked,
        });
    }
    Ok((
        tasks_dto,
        total_retained_bytes,
        eligible_count,
        blocked_count,
    ))
}

fn mint_plan_preview(
    state: &ApiState,
    actor_user_id: UserId,
    plugin: store::plugins::PluginRecord,
    tf: &BoostTaskFactRecord,
    tasks_dto: Vec<CleanupPreviewTaskDto>,
    totals: (u64, usize, usize),
    now: i64,
) -> Result<BrushCleanupPreviewDto, PluginServiceError> {
    if let Some(reason) = tasks_dto[0].blocked_reason.as_deref() {
        return Err(PluginServiceError::conflict(
            "plugin.cleanup_blocked",
            format!("cleanup plan blocked: {reason}"),
        ));
    }
    let plan_handle = tf
        .cleanup_plan_handle
        .clone()
        .ok_or_else(|| PluginServiceError::Io("missing cleanup_plan_handle".into()))?;
    let raw_json = tf.cleanup_plan_json.as_deref().unwrap_or("{}");
    let plan_digest = compute_plan_digest(raw_json);
    let expires_at = tf.cleanup_plan_expires_at.unwrap_or(now + 600);
    let (token, expires_rfc3339) = state.plugins.cleanup_tokens().mint(CleanupTokenBinding {
        actor_user_id,
        plugin_id: plugin.plugin_id,
        package_hash: plugin.package_hash,
        generation: plugin.generation,
        plan_handle: plan_handle.clone(),
        revision: tf.revision,
        plan_digest: plan_digest.clone(),
        expires_at,
    });
    Ok(BrushCleanupPreviewDto {
        mode: "plan",
        plan_handle: Some(plan_handle),
        revision: Some(tf.revision),
        confirmation_token: Some(token),
        expires_at: Some(expires_rfc3339),
        plan_digest: Some(plan_digest),
        total_retained_bytes: totals.0,
        eligible_count: totals.1,
        blocked_count: totals.2,
        tasks: tasks_dto,
    })
}

pub(crate) async fn preview_brush_cleanup(
    state: &ApiState,
    actor_user_id: UserId,
    plugin_id: &str,
    query: CleanupPreviewQuery,
) -> Result<BrushCleanupPreviewDto, PluginServiceError> {
    let now = unix_now();
    let (plugin, selected_tasks, all_tasks, mut protections, is_plan) =
        load_preview_context(state, plugin_id.to_string(), query, now).await?;
    let state_bg = state.clone();
    let (selected_tasks, tasks_dto, total_retained_bytes, eligible_count, blocked_count) =
        tokio::task::spawn_blocking(move || -> Result<_, PluginServiceError> {
            expand_protection_sets_off_lock(&mut protections);
            let (dto, total, elig, blk) =
                build_preview_tasks(&state_bg, &selected_tasks, &all_tasks, &protections, now)?;
            Ok((selected_tasks, dto, total, elig, blk))
        })
        .await
        .map_err(|e| PluginServiceError::Io(e.to_string()))??;

    if is_plan && let Some(tf) = selected_tasks.first() {
        return mint_plan_preview(
            state,
            actor_user_id,
            plugin,
            tf,
            tasks_dto,
            (total_retained_bytes, eligible_count, blocked_count),
            now,
        );
    }

    Ok(BrushCleanupPreviewDto {
        mode: "selection",
        plan_handle: None,
        revision: None,
        confirmation_token: None,
        expires_at: None,
        plan_digest: None,
        total_retained_bytes,
        eligible_count,
        blocked_count,
        tasks: tasks_dto,
    })
}
