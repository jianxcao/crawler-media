use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use domain::{DownloaderId, SiteId};
use downloader::{DiskSpaceStatus, Downloader, TaskSnapshot};
use plugin_api::{BoostDisk, BoostReservationView, BoostSnapshotView, BoostTaskView};
use serde::Serialize;
use store::download_operations::{BoostProtectionSets, BoostSnapshotFacts, BoostTaskFactRecord};
use store::{Db, DownloaderRow, Store};

use crate::delivery;
use crate::job_loop::unix_now;
use crate::management::ApiState;
use crate::plugins::PluginServiceError;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct BrushTaskDto {
    pub handle: String,
    pub task_id: String,
    pub quota_key: String,
    pub downloader_id: String,
    pub site_id: Option<String>,
    pub info_hash: String,
    pub state: String,
    pub task_bytes: u64,
    pub retained_bytes: u64,
    pub uploaded_bytes: u64,
    pub hr_protected: Option<bool>,
    pub hold_until: Option<u64>,
    pub free_until: Option<u64>,
    pub protection_state: String,
    pub transfer_active: bool,
    pub library_referenced: bool,
    pub observed_at: u64,
    pub revision: u64,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct BrushReservationDto {
    pub handle: String,
    pub operation_id: String,
    pub quota_key: String,
    pub downloader_id: String,
    pub site_id: Option<String>,
    pub info_hash: Option<String>,
    pub reserved_bytes: u64,
    pub stage: String,
    pub expires_at: Option<u64>,
    pub revision: u64,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct BrushDiskDto {
    pub available_bytes: Option<u64>,
    pub observed_at: u64,
    pub status: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct BrushStateDto {
    pub revision: u64,
    pub used_bytes: u64,
    pub reserved_bytes: u64,
    pub retained_file_bytes: u64,
    pub reservations: Vec<BrushReservationDto>,
    pub tasks: Vec<BrushTaskDto>,
    pub disk: BrushDiskDto,
}

impl From<&BoostSnapshotView> for BrushStateDto {
    fn from(v: &BoostSnapshotView) -> Self {
        Self {
            revision: v.revision,
            used_bytes: v.used_bytes,
            reserved_bytes: v.reserved_bytes,
            retained_file_bytes: v.retained_file_bytes,
            reservations: v
                .reservations
                .iter()
                .map(|r| BrushReservationDto {
                    handle: r.handle.clone(),
                    operation_id: r.operation_id.clone(),
                    quota_key: r.quota_key.clone(),
                    downloader_id: r.downloader_id.clone(),
                    site_id: r.site_id.clone(),
                    info_hash: r.info_hash.clone(),
                    reserved_bytes: r.reserved_bytes,
                    stage: r.stage.clone(),
                    expires_at: r.expires_at,
                    revision: r.revision,
                })
                .collect(),
            tasks: v
                .tasks
                .iter()
                .map(|t| BrushTaskDto {
                    handle: t.handle.clone(),
                    task_id: t.handle.clone(),
                    quota_key: t.quota_key.clone(),
                    downloader_id: t.downloader_id.clone(),
                    site_id: t.site_id.clone(),
                    info_hash: t.info_hash.clone(),
                    state: t.state.clone(),
                    task_bytes: t.task_bytes,
                    retained_bytes: t.retained_bytes,
                    uploaded_bytes: t.uploaded_bytes,
                    hr_protected: t.hr_protected,
                    hold_until: t.hold_until,
                    free_until: t.free_until,
                    protection_state: t.protection_state.clone(),
                    transfer_active: t.transfer_active,
                    library_referenced: t.library_referenced,
                    observed_at: t.observed_at,
                    revision: t.revision,
                })
                .collect(),
            disk: BrushDiskDto {
                available_bytes: v.disk.available_bytes,
                observed_at: v.disk.observed_at,
                status: v.disk.status.clone(),
            },
        }
    }
}

pub(crate) struct SnapshotQuery<'a> {
    pub plugin_id: &'a str,
    pub site_ids: &'a [SiteId],
    pub downloader_ids: &'a [DownloaderId],
    pub allowed_sites: Option<&'a HashSet<SiteId>>,
    pub allowed_downloaders: Option<&'a HashSet<DownloaderId>>,
}

struct DbSnapshotData {
    facts: BoostSnapshotFacts,
    downloaders: Vec<DownloaderRow>,
    protections: BoostProtectionSets,
}

fn read_db_snapshot(
    store: &Store,
    query: &SnapshotQuery<'_>,
) -> Result<DbSnapshotData, PluginServiceError> {
    let plugin_exists = store.get_plugin(query.plugin_id)?.is_some();
    if !plugin_exists {
        return Err(PluginServiceError::not_found(
            "plugin.missing",
            "plugin not found",
        ));
    }
    store
        .with_read_snapshot(
            &[Db::Library, Db::App, Db::Subscribe],
            Duration::from_secs(2),
            || {
                let downloaders = store.list_downloaders()?;
                let mut facts = store.get_boost_snapshot_for_plugin(
                    query.plugin_id,
                    query.site_ids,
                    query.downloader_ids,
                )?;
                if let Some(allowed_s) = query.allowed_sites {
                    facts
                        .reservations
                        .retain(|r| r.site_id.is_some_and(|s| allowed_s.contains(&s)));
                    facts
                        .tasks
                        .retain(|t| t.site_id.is_some_and(|s| allowed_s.contains(&s)));
                }
                if let Some(allowed_d) = query.allowed_downloaders {
                    facts
                        .reservations
                        .retain(|r| allowed_d.contains(&r.downloader_id));
                    facts.tasks.retain(|t| allowed_d.contains(&t.downloader_id));
                }
                facts.reserved_bytes = facts
                    .reservations
                    .iter()
                    .filter(|r| {
                        matches!(
                            r.stage,
                            store::download_operations::BoostReservationStage::Final
                                | store::download_operations::BoostReservationStage::UnknownHeld
                        )
                    })
                    .try_fold(0i64, |acc, r| acc.checked_add(r.reserved_bytes))
                    .ok_or(store::StoreError::Plugin(
                        store::plugins::PluginError::QuotaExceeded,
                    ))?;
                facts.used_bytes = facts
                    .tasks
                    .iter()
                    .try_fold(0i64, |acc, t| acc.checked_add(t.occupied_bytes))
                    .ok_or(store::StoreError::Plugin(
                        store::plugins::PluginError::QuotaExceeded,
                    ))?;
                facts.retained_file_bytes = facts
                    .tasks
                    .iter()
                    .try_fold(0i64, |acc, t| acc.checked_add(t.retained_bytes))
                    .ok_or(store::StoreError::Plugin(
                        store::plugins::PluginError::QuotaExceeded,
                    ))?;
                let protections = store.load_boost_protection_sets()?;
                Ok(DbSnapshotData {
                    facts,
                    downloaders,
                    protections,
                })
            },
        )
        .map_err(PluginServiceError::Store)?
        .map_err(PluginServiceError::Store)
}

#[cfg(unix)]
pub(crate) fn statvfs_available_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    if !path.exists() {
        return None;
    }
    let raw = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `raw` is NUL-terminated and `stats` is initialized when statvfs returns 0.
    if unsafe { libc::statvfs(raw.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: `statvfs` returned 0, guaranteeing `stats` was initialized.
    let stats = unsafe { stats.assume_init() };
    let block = if stats.f_frsize > 0 {
        stats.f_frsize
    } else {
        stats.f_bsize
    };
    Some((stats.f_bavail as u64).saturating_mul(block))
}

#[cfg(not(unix))]
pub(crate) fn statvfs_available_bytes(_path: &Path) -> Option<u64> {
    None
}

pub(crate) fn extract_plan_paths(tf: &BoostTaskFactRecord) -> Vec<String> {
    let Some(raw) = tf.cleanup_plan_json.as_deref() else {
        return Vec::new();
    };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    val.get("paths")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

pub(crate) fn expand_path_set_off_lock(set: &mut HashSet<String>) {
    let raw: Vec<String> = set.iter().cloned().collect();
    for p in raw {
        let trimmed = p.trim();
        if trimmed.is_empty() {
            continue;
        }
        set.insert(store::path_key::normalize_path_key(trimmed));
        set.insert(store::path_key::canonical_existing_key(trimmed));
    }
}

pub(crate) fn expand_protection_sets_off_lock(protections: &mut BoostProtectionSets) {
    expand_path_set_off_lock(&mut protections.library_paths);
    expand_path_set_off_lock(&mut protections.transfer_paths);
}

fn key_or_non_root_ancestor_in_set(key: &str, set: &HashSet<String>) -> bool {
    let mut cur = Path::new(key);
    loop {
        if cur.parent().is_none() {
            return false;
        }
        if let Some(s) = cur.to_str()
            && !s.is_empty()
            && set.contains(s)
        {
            return true;
        }
        let Some(parent) = cur.parent() else {
            return false;
        };
        cur = parent;
    }
}

fn canonical_or_ancestor_key(norm: &str) -> String {
    let direct = store::path_key::canonical_existing_key(norm);
    if direct != norm {
        return direct;
    }
    let mut cur = Path::new(norm);
    let mut tail = Vec::new();
    while let Some(parent) = cur.parent() {
        let Some(std::path::Component::Normal(seg)) = cur.components().next_back() else {
            break;
        };
        tail.push(seg.to_os_string());
        if let Ok(mut real) = std::fs::canonicalize(parent) {
            for part in tail.into_iter().rev() {
                real.push(part);
            }
            return store::path_key::normalize_path_key(&real.to_string_lossy());
        }
        cur = parent;
    }
    direct
}

pub(crate) fn path_is_in_set(path: &str, set: &HashSet<String>) -> bool {
    if set.is_empty() || path.trim().is_empty() {
        return false;
    }
    let trimmed = path.trim_end_matches('/');
    if !trimmed.is_empty() && (set.contains(path) || set.contains(trimmed)) {
        return true;
    }
    let norm = store::path_key::normalize_path_key(path);
    if key_or_non_root_ancestor_in_set(&norm, set) {
        return true;
    }
    let canon = canonical_or_ancestor_key(&norm);
    canon != norm && key_or_non_root_ancestor_in_set(&canon, set)
}

struct ProbedDownloaderMetrics {
    snap_by_dl: HashMap<DownloaderId, HashMap<String, TaskSnapshot>>,
    files_by_task: HashMap<(DownloaderId, String), Vec<String>>,
    disk: BoostDisk,
}

fn resolve_enabled_probe_targets(
    db: &DbSnapshotData,
    query: &SnapshotQuery<'_>,
) -> Vec<Option<DownloaderId>> {
    let nil_id = DownloaderId::from_uuid(uuid::Uuid::nil());
    let disabled_ids: HashSet<DownloaderId> = db
        .downloaders
        .iter()
        .filter(|d| !d.enabled)
        .map(|d| d.id)
        .collect();
    if !query.downloader_ids.is_empty() {
        let mut out = Vec::new();
        for &id in query.downloader_ids {
            if id == nil_id {
                let default_disabled = db.downloaders.iter().any(|d| d.is_default && !d.enabled);
                if !default_disabled
                    && query
                        .allowed_downloaders
                        .is_none_or(|a| a.contains(&nil_id))
                    && !out.contains(&None)
                {
                    out.push(None);
                }
                continue;
            }
            if disabled_ids.contains(&id) {
                continue;
            }
            if query
                .allowed_downloaders
                .is_some_and(|allowed| !allowed.contains(&id))
            {
                continue;
            }
            if !out.contains(&Some(id)) {
                out.push(Some(id));
            }
        }
        return out;
    }

    let default_disabled = db.downloaders.iter().any(|d| d.is_default && !d.enabled);
    let mut target_dls: Vec<DownloaderId> = db
        .downloaders
        .iter()
        .filter(|d| d.enabled)
        .map(|d| d.id)
        .collect();
    if !default_disabled
        && !target_dls.contains(&nil_id)
        && db.facts.tasks.iter().any(|t| t.downloader_id == nil_id)
    {
        target_dls.push(nil_id);
    }
    if let Some(allowed) = query.allowed_downloaders {
        target_dls.retain(|id| allowed.contains(id));
        return target_dls.into_iter().map(Some).collect();
    }
    if target_dls.is_empty() {
        if default_disabled {
            Vec::new()
        } else {
            vec![None]
        }
    } else {
        target_dls.into_iter().map(Some).collect()
    }
}

fn collect_downloader_metrics(
    store: &Store,
    default_dl: &Arc<dyn Downloader>,
    db: &DbSnapshotData,
    query: &SnapshotQuery<'_>,
) -> ProbedDownloaderMetrics {
    let probe_targets = resolve_enabled_probe_targets(db, query);
    let no_probe_targets = probe_targets.is_empty();
    let mut snap_by_dl: HashMap<DownloaderId, HashMap<String, TaskSnapshot>> = HashMap::new();
    let mut files_by_task: HashMap<(DownloaderId, String), Vec<String>> = HashMap::new();
    let mut any_available = false;
    let mut any_unavailable = false;
    let mut min_free: Option<u64> = None;

    for dl_opt in probe_targets {
        let Ok(client) = delivery::client_for_store_id(store, default_dl, dl_opt) else {
            any_unavailable = true;
            continue;
        };
        match client.disk_free_space() {
            Ok(info) => match info.status {
                DiskSpaceStatus::Available => {
                    if let Some(bytes) = info.available_bytes {
                        any_available = true;
                        min_free = Some(min_free.map_or(bytes, |m| m.min(bytes)));
                    }
                }
                DiskSpaceStatus::Unknown => {
                    if let Some(bytes) = info.root_path.as_deref().and_then(statvfs_available_bytes)
                    {
                        any_available = true;
                        min_free = Some(min_free.map_or(bytes, |m| m.min(bytes)));
                    }
                }
                DiskSpaceStatus::Unavailable => any_unavailable = true,
            },
            Err(_) => any_unavailable = true,
        }
        let dl_id = dl_opt.unwrap_or_else(|| DownloaderId::from_uuid(uuid::Uuid::nil()));
        if let Ok(snaps) = client.task_snapshots() {
            let map = snaps
                .into_iter()
                .map(|s| (s.info_hash.trim().to_ascii_lowercase(), s))
                .collect();
            snap_by_dl.insert(dl_id, map);
        }
        let path_maps = db
            .downloaders
            .iter()
            .find(|d| d.id == dl_id)
            .map(|d| d.path_maps.as_slice())
            .unwrap_or(&[]);
        for t in db.facts.tasks.iter().filter(|t| t.downloader_id == dl_id) {
            let hash_norm = t.info_hash.trim().to_ascii_lowercase();
            if let Ok(Some(fset)) = client.task_files_by_hash(&hash_norm) {
                let paths = fset
                    .files
                    .iter()
                    .map(|p| {
                        crate::download_service::plugin_command_ops::normalize_task_file_path(
                            &fset.save_path,
                            p,
                            path_maps,
                        )
                    })
                    .collect();
                files_by_task.insert((dl_id, hash_norm), paths);
            }
        }
    }

    let observed_at = u64::try_from(unix_now()).unwrap_or(0);
    let status = if any_available {
        "available"
    } else if any_unavailable || no_probe_targets {
        "unavailable"
    } else {
        "unknown"
    };
    ProbedDownloaderMetrics {
        snap_by_dl,
        files_by_task,
        disk: BoostDisk {
            available_bytes: min_free,
            observed_at,
            status: status.to_string(),
        },
    }
}

fn probe_downloaders_off_lock(
    store: &Store,
    default_dl: &Arc<dyn Downloader>,
    db: &DbSnapshotData,
    query: &SnapshotQuery<'_>,
) -> BoostSnapshotView {
    let probed = collect_downloader_metrics(store, default_dl, db, query);
    let observed_at = probed.disk.observed_at;

    let reservations = db
        .facts
        .reservations
        .iter()
        .map(|r| BoostReservationView {
            handle: r.reservation_id.clone(),
            operation_id: r.operation_id.clone(),
            quota_key: r.quota_key.clone(),
            downloader_id: r.downloader_id.to_string(),
            site_id: r.site_id.map(|s| s.to_string()),
            info_hash: r.info_hash.clone(),
            reserved_bytes: u64::try_from(r.reserved_bytes).unwrap_or(0),
            stage: r.stage.as_str().to_string(),
            expires_at: r.expires_at.and_then(|v| u64::try_from(v).ok()),
            revision: u64::try_from(r.revision).unwrap_or(1),
        })
        .collect::<Vec<_>>();

    let mut max_rev: u64 = 1;
    let tasks = db
        .facts
        .tasks
        .iter()
        .map(|t| {
            let hash_norm = t.info_hash.trim().to_ascii_lowercase();
            let live = probed
                .snap_by_dl
                .get(&t.downloader_id)
                .and_then(|m| m.get(&hash_norm));
            let uploaded_bytes = live.map_or(0, |s| s.uploaded_bytes);
            let mut candidate_paths = probed
                .files_by_task
                .get(&(t.downloader_id, hash_norm.clone()))
                .cloned()
                .unwrap_or_default();
            if candidate_paths.is_empty() {
                candidate_paths = extract_plan_paths(t);
            }
            let library_referenced = candidate_paths
                .iter()
                .any(|p| path_is_in_set(p, &db.protections.library_paths));
            let transfer_active = db.protections.active_subscribe_hashes.contains(&hash_norm)
                || candidate_paths
                    .iter()
                    .any(|p| path_is_in_set(p, &db.protections.transfer_paths));
            let rev = u64::try_from(t.revision).unwrap_or(1);
            max_rev = max_rev.max(rev);
            let protection_state = if library_referenced || transfer_active {
                "protected".to_string()
            } else {
                t.protection_state.as_str().to_string()
            };
            BoostTaskView {
                handle: t.task_fact_id.clone(),
                quota_key: t.quota_key.clone(),
                downloader_id: t.downloader_id.to_string(),
                site_id: t.site_id.map(|s| s.to_string()),
                info_hash: t.info_hash.clone(),
                state: t.state.as_str().to_string(),
                task_bytes: u64::try_from(t.occupied_bytes).unwrap_or(0),
                retained_bytes: u64::try_from(t.retained_bytes).unwrap_or(0),
                uploaded_bytes,
                hr_protected: t.hr_active,
                hold_until: t.hold_until.and_then(|v| u64::try_from(v).ok()),
                free_until: t.free_until.and_then(|v| u64::try_from(v).ok()),
                protection_state,
                transfer_active,
                library_referenced,
                observed_at: u64::try_from(t.observed_at).unwrap_or(observed_at),
                revision: rev,
            }
        })
        .collect::<Vec<_>>();

    BoostSnapshotView {
        revision: max_rev,
        used_bytes: u64::try_from(db.facts.used_bytes).unwrap_or(0),
        reserved_bytes: u64::try_from(db.facts.reserved_bytes).unwrap_or(0),
        retained_file_bytes: u64::try_from(db.facts.retained_file_bytes).unwrap_or(0),
        reservations,
        tasks,
        disk: probed.disk,
    }
}

pub(crate) fn collect_boost_snapshot_sync(
    store: &Store,
    default_dl: &Arc<dyn Downloader>,
    query: &SnapshotQuery<'_>,
) -> Result<BoostSnapshotView, PluginServiceError> {
    let mut db = read_db_snapshot(store, query)?;
    expand_protection_sets_off_lock(&mut db.protections);
    Ok(probe_downloaders_off_lock(store, default_dl, &db, query))
}

pub(crate) async fn get_brush_state_http(
    state: &ApiState,
    plugin_id: &str,
    site_id: Option<SiteId>,
    downloader_id: Option<DownloaderId>,
) -> Result<BrushStateDto, PluginServiceError> {
    let pid = plugin_id.to_string();
    let sids: Vec<SiteId> = site_id.into_iter().collect();
    let dl_ids: Vec<DownloaderId> = downloader_id.into_iter().collect();
    let st_read = state.clone();
    let mut db = tokio::task::spawn_blocking(move || {
        read_db_snapshot(
            &st_read.store,
            &SnapshotQuery {
                plugin_id: &pid,
                site_ids: &sids,
                downloader_ids: &dl_ids,
                allowed_sites: None,
                allowed_downloaders: None,
            },
        )
    })
    .await
    .map_err(|e| PluginServiceError::Io(e.to_string()))??;

    let st_http = state.clone();
    let pid2 = plugin_id.to_string();
    let sids2: Vec<SiteId> = site_id.into_iter().collect();
    let dl_ids2: Vec<DownloaderId> = downloader_id.into_iter().collect();
    let view = state
        .http_work
        .run(Duration::from_secs(10), move || {
            expand_protection_sets_off_lock(&mut db.protections);
            probe_downloaders_off_lock(
                &st_http.store,
                &st_http.downloader,
                &db,
                &SnapshotQuery {
                    plugin_id: &pid2,
                    site_ids: &sids2,
                    downloader_ids: &dl_ids2,
                    allowed_sites: None,
                    allowed_downloaders: None,
                },
            )
        })
        .await
        .map_err(|err| match err {
            crate::blocking_budget::BlockingError::Busy => PluginServiceError::Busy {
                code: "runtime.busy",
                message: "downloader probe busy".into(),
            },
            other => PluginServiceError::Io(other.to_string()),
        })?;
    Ok(BrushStateDto::from(&view))
}
