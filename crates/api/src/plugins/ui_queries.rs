use std::collections::BTreeSet;
use std::str::FromStr;

use axum::http::HeaderMap;
use domain::{DownloaderId, MediaId, SiteId, UserId};
use serde::Deserialize;
use serde_json::{Value, json};
use store::Db;
use store::plugins::{PluginLifecycle, PluginRecord};
use uuid::Uuid;

use super::boost::get_brush_state_http;
use super::service::{PluginService, run_app_read};
use super::types::PluginServiceError;
use super::ui_ops::{evaluate_trust, parse_manifest_with_ui};
use super::ui_query_readers::{
    query_checkin_receipts, query_config_options, query_downloaders_list, query_downloads_list,
    query_filters_list, query_libraries_list, query_media_get, query_sites_list,
    query_subscribes_list, run_db_read,
};
use crate::management::ApiState;

const DEFAULT_QUERY_LIMIT: usize = 50;
const MAX_QUERY_LIMIT: usize = 200;

#[derive(Deserialize)]
struct LimitCursorInput {
    limit: Option<usize>,
    #[allow(dead_code)]
    cursor: Option<String>,
}

#[derive(Deserialize)]
struct SubscribesListInput {
    media_id: Option<MediaId>,
    cursor: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct DownloadsListInput {
    site_id: Option<SiteId>,
    downloader_id: Option<DownloaderId>,
    cursor: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct ConfigOptionsInput {
    kind: String,
    limit: Option<usize>,
    #[allow(dead_code)]
    cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RevisionInput {
    Int(i64),
    Str(String),
}

impl RevisionInput {
    fn into_i64(self) -> Result<i64, PluginServiceError> {
        match self {
            Self::Int(n) => Ok(n),
            Self::Str(s) => s.trim().parse::<i64>().map_err(|_| {
                PluginServiceError::invalid(
                    "plugin.invalid_input",
                    "expected_revision must be an integer or numeric string",
                )
            }),
        }
    }
}

#[derive(Deserialize)]
struct HandleResolveInput {
    kind: String,
    id: String,
    expected_revision: Option<RevisionInput>,
}

#[derive(Deserialize)]
struct IndexerCachedInput {
    search_operation_id: Uuid,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct CheckinReceiptsInput {
    site_id: Option<SiteId>,
    cursor: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct BoostSnapshotInput {
    site_id: Option<SiteId>,
    downloader_id: Option<DownloaderId>,
}

#[derive(Deserialize)]
struct MediaGetInput {
    id: Option<MediaId>,
    media_id: Option<MediaId>,
}

#[derive(Deserialize)]
struct FilesSelectionInput {
    download_task_handle: Option<String>,
    torrent_handle: Option<String>,
    library_id: Option<domain::LibraryId>,
    file_ids: Option<Vec<String>>,
    expected_revisions: Option<Vec<RevisionInput>>,
    limit: Option<usize>,
}

fn validate_limit(limit: Option<usize>) -> Result<usize, PluginServiceError> {
    match limit {
        None => Ok(DEFAULT_QUERY_LIMIT),
        Some(n) if (1..=MAX_QUERY_LIMIT).contains(&n) => Ok(n),
        Some(_) => Err(PluginServiceError::invalid(
            "plugin.invalid_input",
            "limit must be between 1 and 200",
        )),
    }
}

pub(super) fn require_any_ui_cap(
    caps: &BTreeSet<String>,
    required: &[&str],
) -> Result<(), PluginServiceError> {
    if required.iter().any(|c| caps.contains(*c)) {
        Ok(())
    } else {
        Err(PluginServiceError::forbidden(
            "plugin.capability_denied",
            format!(
                "plugin UI lacks required capability ({})",
                required.join(" | ")
            ),
        ))
    }
}

fn parse_input<T: serde::de::DeserializeOwned>(input: Value) -> Result<T, PluginServiceError> {
    serde_json::from_value(input)
        .map_err(|e| PluginServiceError::invalid("plugin.invalid_input", e.to_string()))
}

impl PluginService {
    pub(crate) async fn execute_ui_query(
        &self,
        state: &ApiState,
        plugin_id: &str,
        actor_user_id: UserId,
        headers: &HeaderMap,
        name: &str,
        input: Value,
    ) -> Result<Value, PluginServiceError> {
        let plugin = if name == "config.options" {
            let (plugin, _) = self
                .verify_config_attribution(state, plugin_id, headers)
                .await?;
            if matches!(
                plugin.lifecycle,
                PluginLifecycle::Draining | PluginLifecycle::Enabling
            ) || plugin.active_operation_id.is_some()
            {
                return Err(PluginServiceError::conflict(
                    "plugin.not_drained",
                    "plugin operation in progress",
                ));
            }
            plugin
        } else {
            let (plugin, _) = self
                .verify_plugin_attribution(state, plugin_id, headers)
                .await?;
            if plugin.lifecycle != PluginLifecycle::Enabled || !plugin.desired_enabled {
                return Err(PluginServiceError::forbidden(
                    "plugin.not_enabled",
                    "plugin must be enabled to run UI queries",
                ));
            }
            plugin
        };
        let pid = plugin.plugin_id.clone();
        let pkg_hash = plugin.package_hash.clone();
        let (version, trust_row) = run_app_read(state, move |store| {
            let ver = store.get_plugin_version(&pid, &pkg_hash)?.ok_or_else(|| {
                PluginServiceError::not_found("plugin.missing", "plugin version not found")
            })?;
            let trust = store.get_ui_trust(&pid, &pkg_hash)?;
            Ok((ver, trust))
        })
        .await?;
        let (_, ui) = parse_manifest_with_ui(&version)?;
        if name == "config.options" {
            let is_enabled_idle = plugin.lifecycle == PluginLifecycle::Enabled
                && plugin.desired_enabled
                && plugin.active_operation_id.is_none();
            if !is_enabled_idle && !super::ui_ops::can_serve_disabled_config_ui(&plugin, &ui) {
                return Err(PluginServiceError::forbidden(
                    "plugin.not_enabled",
                    "plugin Config UI is not enabled or available",
                ));
            }
        }
        let (trusted, _) = evaluate_trust(&version, trust_row.as_ref());
        if !trusted {
            return Err(PluginServiceError::forbidden(
                "plugin.ui_untrusted",
                "plugin UI must be trusted before executing UI queries",
            ));
        }
        let caps: BTreeSet<String> = ui.capabilities.into_iter().collect();
        self.dispatch_ui_query(state, &plugin, actor_user_id, &caps, name, input)
            .await
    }

    async fn dispatch_ui_query(
        &self,
        state: &ApiState,
        plugin: &PluginRecord,
        actor_user_id: UserId,
        caps: &BTreeSet<String>,
        name: &str,
        input: Value,
    ) -> Result<Value, PluginServiceError> {
        match name {
            "config.options" => {
                let q: ConfigOptionsInput = parse_input(input)?;
                query_config_options(state, caps, &q.kind, validate_limit(q.limit)?).await
            }
            "sites.list" => {
                require_any_ui_cap(caps, &["read:sites"])?;
                let q: LimitCursorInput = parse_input(input)?;
                query_sites_list(state, validate_limit(q.limit)?).await
            }
            "downloaders.list" => {
                require_any_ui_cap(caps, &["read:downloads", "read:downloaders"])?;
                let q: LimitCursorInput = parse_input(input)?;
                query_downloaders_list(state, validate_limit(q.limit)?).await
            }
            "libraries.list" => {
                require_any_ui_cap(caps, &["read:library", "read:libraries"])?;
                let q: LimitCursorInput = parse_input(input)?;
                query_libraries_list(state, validate_limit(q.limit)?).await
            }
            "filters.list" => {
                require_any_ui_cap(caps, &["read:subscribes", "read:sites", "read:catalog"])?;
                let q: LimitCursorInput = parse_input(input)?;
                query_filters_list(state, validate_limit(q.limit)?).await
            }
            "subscribes.list" => {
                require_any_ui_cap(caps, &["read:subscribes", "read:catalog"])?;
                let q: SubscribesListInput = parse_input(input)?;
                query_subscribes_list(
                    state,
                    actor_user_id,
                    q.media_id,
                    q.cursor,
                    validate_limit(q.limit)?,
                )
                .await
            }
            "media.get" => {
                require_any_ui_cap(caps, &["read:catalog", "read:library"])?;
                let q: MediaGetInput = parse_input(input)?;
                let media_id = q.id.or(q.media_id).ok_or_else(|| {
                    PluginServiceError::invalid("plugin.invalid_input", "media id is required")
                })?;
                query_media_get(state, media_id).await
            }
            other => {
                self.dispatch_ui_operational_query(state, plugin, actor_user_id, caps, other, input)
                    .await
            }
        }
    }

    async fn dispatch_ui_operational_query(
        &self,
        state: &ApiState,
        plugin: &PluginRecord,
        actor_user_id: UserId,
        caps: &BTreeSet<String>,
        name: &str,
        input: Value,
    ) -> Result<Value, PluginServiceError> {
        match name {
            "downloads.list" => {
                require_any_ui_cap(caps, &["read:downloads", "read:downloaders", "read:boost"])?;
                let q: DownloadsListInput = parse_input(input)?;
                query_downloads_list(
                    state,
                    q.site_id,
                    q.downloader_id,
                    validate_limit(q.limit)?,
                    q.cursor,
                )
                .await
            }
            "checkin.receipts" => {
                require_any_ui_cap(caps, &["read:sites", "read:history"])?;
                let q: CheckinReceiptsInput = parse_input(input)?;
                query_checkin_receipts(state, q.site_id, q.cursor, validate_limit(q.limit)?).await
            }
            "boost.snapshot" => {
                require_any_ui_cap(caps, &["read:boost"])?;
                let q: BoostSnapshotInput = parse_input(input)?;
                let snap =
                    get_brush_state_http(state, &plugin.plugin_id, q.site_id, q.downloader_id)
                        .await?;
                serde_json::to_value(snap).map_err(|e| PluginServiceError::Io(e.to_string()))
            }
            "handles.resolve" => {
                let q: HandleResolveInput = parse_input(input)?;
                self.resolve_ui_handle(
                    state,
                    caps,
                    &plugin.plugin_id,
                    plugin.generation,
                    actor_user_id,
                    q,
                )
                .await
            }
            "files.selection" => {
                require_any_ui_cap(
                    caps,
                    &[
                        "read:boost",
                        "read:downloads",
                        "read:library",
                        "read:libraries",
                    ],
                )?;
                let q: FilesSelectionInput = parse_input(input)?;
                self.query_files_selection(state, caps, plugin, actor_user_id, q)
                    .await
            }
            "indexer.cached" => {
                require_any_ui_cap(caps, &["read:catalog", "read:sites"])?;
                let q: IndexerCachedInput = parse_input(input)?;
                super::ui_indexer::query_indexer_cached(
                    state,
                    plugin,
                    actor_user_id,
                    q.search_operation_id,
                    validate_limit(q.limit)?,
                )
                .await
            }
            other => Err(PluginServiceError::invalid(
                "plugin.unknown_ui_query",
                format!("unsupported ui query `{other}`"),
            )),
        }
    }

    async fn query_files_selection(
        &self,
        state: &ApiState,
        caps: &BTreeSet<String>,
        plugin: &PluginRecord,
        actor_user_id: UserId,
        q: FilesSelectionInput,
    ) -> Result<Value, PluginServiceError> {
        if let Some(raw_ids) = q.file_ids {
            require_any_ui_cap(caps, &["read:library", "read:libraries"])?;
            let lib_id = q.library_id.ok_or_else(|| {
                PluginServiceError::invalid("plugin.invalid_input", "library_id is required")
            })?;
            return self
                .mint_library_file_selection_handle(
                    state,
                    plugin,
                    actor_user_id,
                    lib_id,
                    raw_ids,
                    q.expected_revisions,
                )
                .await;
        }
        let _limit = validate_limit(q.limit)?;
        let mut target_kind = None;
        let mut target_id = None;
        if let Some(h) = q.download_task_handle.as_deref() {
            let v = self.verify_ui_handle(
                &plugin.plugin_id,
                plugin.generation,
                actor_user_id,
                Some("download_task"),
                h,
            )?;
            target_kind = Some(v.entity_kind);
            target_id = Some(v.entity_id);
        } else if let Some(h) = q.torrent_handle.as_deref() {
            let v = self.verify_ui_handle(
                &plugin.plugin_id,
                plugin.generation,
                actor_user_id,
                Some("torrent"),
                h,
            )?;
            target_kind = Some(v.entity_kind);
            target_id = Some(v.entity_id);
        }
        Ok(json!({
            "kind": target_kind,
            "entity_id": target_id,
            "library_id": q.library_id.map(|id| id.to_string()),
            "files": []
        }))
    }

    async fn mint_library_file_selection_handle(
        &self,
        state: &ApiState,
        plugin: &PluginRecord,
        actor_user_id: UserId,
        library_id: domain::LibraryId,
        raw_ids: Vec<String>,
        expected_revisions: Option<Vec<RevisionInput>>,
    ) -> Result<Value, PluginServiceError> {
        use sha2::{Digest, Sha256};

        let (file_ids, exp_revs) = validate_selection_ids_and_revs(&raw_ids, expected_revisions)?;
        let lib_str = library_id.to_string();
        let ids_for_db = file_ids.clone();
        let combined_rev = run_db_read(state, &[Db::Library], move |store| {
            let lib_opt = store.get_library(&lib_str)?;
            let accepts = store.library_accepts_work(&lib_str)?;
            let mut revs = Vec::with_capacity(ids_for_db.len());
            for fid in &ids_for_db {
                if let Some(cov) = store.file_coverage_by_id(*fid)?
                    && cov.file.library_id == library_id
                {
                    revs.push(cov.file.version.revision);
                }
            }
            Ok((lib_opt, accepts, revs))
        })
        .await
        .and_then(|(lib_opt, accepts, revs)| {
            lib_opt.ok_or_else(|| {
                PluginServiceError::not_found("library.missing", "library not found")
            })?;
            if !accepts {
                return Err(PluginServiceError::conflict(
                    "library.deleting",
                    "library does not accept work",
                ));
            }
            if revs.len() != file_ids.len() {
                return Err(PluginServiceError::not_found(
                    "file.missing",
                    "one or more selected files were not found in library",
                ));
            }
            let mut sum_rev = 0i64;
            for (idx, rev) in revs.into_iter().enumerate() {
                if let Some(&exp) = exp_revs.get(idx)
                    && exp != rev
                {
                    return Err(PluginServiceError::conflict(
                        "plugin.revision_conflict",
                        "file revision mismatch",
                    ));
                }
                sum_rev = sum_rev.saturating_add(rev);
            }
            Ok(sum_rev)
        })?;
        let mut hasher = Sha256::new();
        hasher.update(library_id.to_string().as_bytes());
        for fid in &file_ids {
            hasher.update(b":");
            hasher.update(fid.to_string().as_bytes());
        }
        let digest: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let entity_id = format!("{library_id}:{digest}");
        let uid = actor_user_id.to_string();
        let (handle, expires_at) = self.ui_handles().mint(
            &plugin.plugin_id,
            plugin.generation,
            &uid,
            "file_selection",
            &entity_id,
            combined_rev,
        )?;
        Ok(json!({
            "handle": handle,
            "kind": "file_selection",
            "revision": combined_rev.to_string(),
            "expires_at": expires_at,
        }))
    }

    async fn resolve_ui_handle(
        &self,
        state: &ApiState,
        caps: &BTreeSet<String>,
        plugin_id: &str,
        generation: i64,
        actor_user_id: UserId,
        q: HandleResolveInput,
    ) -> Result<Value, PluginServiceError> {
        let expected_rev = q
            .expected_revision
            .map(RevisionInput::into_i64)
            .transpose()?;
        let kind = q.kind.trim().to_string();
        require_handle_kind_cap(caps, &kind)?;
        let raw_id = q.id.trim().to_string();
        let revision = resolve_entity_revision(state, plugin_id, &kind, &raw_id).await?;
        if let Some(exp) = expected_rev
            && exp != revision
        {
            return Err(PluginServiceError::conflict(
                "plugin.revision_conflict",
                "entity revision mismatch",
            ));
        }
        let uid = actor_user_id.to_string();
        let (handle, expires_at) = self
            .ui_handles()
            .mint(plugin_id, generation, &uid, &kind, &raw_id, revision)?;
        Ok(json!({
            "handle": handle,
            "kind": kind,
            "revision": revision,
            "expires_at": expires_at,
        }))
    }
}

fn validate_selection_ids_and_revs(
    raw_ids: &[String],
    expected_revisions: Option<Vec<RevisionInput>>,
) -> Result<(Vec<domain::LedgerId>, Vec<i64>), PluginServiceError> {
    if raw_ids.is_empty() || raw_ids.len() > MAX_QUERY_LIMIT {
        return Err(PluginServiceError::invalid(
            "plugin.invalid_input",
            "file_ids must contain between 1 and 200 items",
        ));
    }
    let mut file_ids = Vec::with_capacity(raw_ids.len());
    for raw in raw_ids {
        let trimmed = raw.trim();
        if trimmed.contains('/') || trimmed.contains('\\') || trimmed.chars().any(char::is_control)
        {
            return Err(PluginServiceError::invalid(
                "plugin.invalid_input",
                "file_ids must be ledger IDs, not filesystem paths",
            ));
        }
        let fid = domain::LedgerId::from_str(trimmed)
            .map_err(|_| PluginServiceError::invalid("plugin.invalid_input", "invalid file_id"))?;
        file_ids.push(fid);
    }
    let exp_revs = match expected_revisions {
        Some(revs) => {
            if revs.len() != file_ids.len() {
                return Err(PluginServiceError::invalid(
                    "plugin.invalid_input",
                    "expected_revisions length must match file_ids",
                ));
            }
            revs.into_iter()
                .map(RevisionInput::into_i64)
                .collect::<Result<Vec<_>, _>>()?
        }
        None => Vec::new(),
    };
    Ok((file_ids, exp_revs))
}

fn require_handle_kind_cap(caps: &BTreeSet<String>, kind: &str) -> Result<(), PluginServiceError> {
    match kind {
        "site" => require_any_ui_cap(caps, &["read:sites"]),
        "downloader" | "owned_download" | "download_task" => {
            require_any_ui_cap(caps, &["read:downloads", "read:downloaders", "read:boost"])
        }
        "library" => require_any_ui_cap(caps, &["read:library", "read:libraries"]),
        "media" => require_any_ui_cap(caps, &["read:library", "read:catalog"]),
        "subscribe" => require_any_ui_cap(caps, &["read:subscribes", "read:catalog"]),
        other => Err(PluginServiceError::invalid(
            "plugin.invalid_input",
            format!("unsupported handle kind `{other}`"),
        )),
    }
}

async fn resolve_entity_revision(
    state: &ApiState,
    plugin_id: &str,
    kind: &str,
    raw_id: &str,
) -> Result<i64, PluginServiceError> {
    match kind {
        "site" => {
            let sid = SiteId::from_str(raw_id).map_err(|_| {
                PluginServiceError::invalid("plugin.invalid_input", "invalid site id")
            })?;
            let obs = run_app_read(state, move |store| Ok(store.observe_site(sid)?))
                .await?
                .ok_or_else(|| PluginServiceError::not_found("site.missing", "site not found"))?;
            Ok(obs.revision())
        }
        "downloader" => {
            let did = DownloaderId::from_str(raw_id).map_err(|_| {
                PluginServiceError::invalid("plugin.invalid_input", "invalid downloader id")
            })?;
            let _ = run_app_read(state, move |store| Ok(store.get_downloader(did)?))
                .await?
                .ok_or_else(|| {
                    PluginServiceError::not_found("downloader.missing", "downloader not found")
                })?;
            Ok(1)
        }
        "library" => {
            let lid = domain::LibraryId::from_str(raw_id).map_err(|_| {
                PluginServiceError::invalid("plugin.invalid_input", "invalid library id")
            })?;
            let lid_s = lid.to_string();
            run_db_read(state, &[Db::Library], move |store| {
                store.get_library(&lid_s)
            })
            .await?
            .ok_or_else(|| PluginServiceError::not_found("library.missing", "library not found"))?;
            Ok(1)
        }
        "media" => {
            let mid = MediaId::from_str(raw_id).map_err(|_| {
                PluginServiceError::invalid("plugin.invalid_input", "invalid media id")
            })?;
            run_db_read(state, &[Db::Library], move |store| store.get_media(mid))
                .await?
                .ok_or_else(|| PluginServiceError::not_found("media.missing", "media not found"))?;
            Ok(1)
        }
        "subscribe" => {
            let sid = domain::SubscribeId::from_str(raw_id).map_err(|_| {
                PluginServiceError::invalid("plugin.invalid_input", "invalid subscribe id")
            })?;
            run_db_read(state, &[Db::Subscribe], move |store| {
                store.get_subscribe(sid)
            })
            .await?
            .ok_or_else(|| {
                PluginServiceError::not_found("subscribe.missing", "subscribe not found")
            })?;
            Ok(1)
        }
        "owned_download" | "download_task" => {
            resolve_download_entity_revision(state, plugin_id, kind, raw_id).await
        }
        other => Err(PluginServiceError::invalid(
            "plugin.invalid_input",
            format!("unsupported handle kind `{other}`"),
        )),
    }
}

async fn resolve_download_entity_revision(
    state: &ApiState,
    plugin_id: &str,
    kind: &str,
    raw_id: &str,
) -> Result<i64, PluginServiceError> {
    if raw_id.is_empty() || raw_id.len() > 256 {
        return Err(PluginServiceError::invalid(
            "plugin.invalid_input",
            "invalid handle target id",
        ));
    }
    let pid = plugin_id.to_string();
    let id_owned = raw_id.to_string();
    let is_owned_only = kind == "owned_download";
    run_db_read(state, &[Db::Subscribe], move |store| {
        if !is_owned_only && let Some(op) = store.get_download_operation(&id_owned)? {
            return Ok(Some(op.revision));
        }
        if let Some(tf) = store.find_boost_task_fact_by_handle(&pid, &id_owned)? {
            return Ok(Some(tf.revision));
        }
        let owned = store.list_owned_download_tasks()?;
        Ok(owned
            .into_iter()
            .find(|t| {
                t.plugin_id.as_deref() == Some(pid.as_str())
                    && (t.task_id == id_owned
                        || t.ownership_tag == id_owned
                        || t.operation_id == id_owned)
            })
            .map(|_| 1))
    })
    .await?
    .ok_or_else(|| {
        PluginServiceError::not_found(
            "plugin.task_missing",
            "owned download task not found for plugin",
        )
    })
}
