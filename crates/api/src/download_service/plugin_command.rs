use std::str::FromStr;

use domain::{DownloaderId, PluginOperationId, SiteId, Torrent};
use downloader::{
    DownloadOpContext, DownloadOutcome, FrozenDownloadPlan, SubmissionPurpose, TorrentSource,
};
use serde::Deserialize;
use serde_json::Value;
use store::DownloadOperationState;
use store::plugins::{PluginCommandTarget, PluginReconciliationOutcome};
use uuid::Uuid;

use super::ApiDownloadService;
use super::reconcile::reconcile_download_operation_by_id;
use crate::management::ApiState;
use crate::plugins::command_targets::{
    grant_scopes_cover_downloader, plugin_grants_cover_download_site, targets_cover_downloader,
};
use crate::plugins::effect::sanitize_phase_token;
use crate::plugins::{ExternalEffectStep, PluginEffectContext, PluginServiceError};

#[derive(Deserialize)]
struct DownloaderAddPayload {
    downloader_id: String,
    #[serde(alias = "download_url")]
    download_ref: String,
    #[serde(default)]
    site_id: Option<String>,
    #[serde(default)]
    pub torrent_id: Option<String>,
    #[serde(default)]
    pub hr: Option<bool>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default, alias = "size_bytes")]
    advertised_size_bytes: Option<u64>,
    #[serde(default)]
    budget_bytes: Option<i64>,
    #[serde(default)]
    relative_directory: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    paused: bool,
    #[serde(default)]
    upload_limit: Option<u64>,
    #[serde(default)]
    download_limit: Option<u64>,
    #[serde(default)]
    sequential: bool,
    #[serde(default)]
    first_last_piece: bool,
}

fn derived_download_op_id(plugin_op_id: PluginOperationId, attempt: i64) -> PluginOperationId {
    let salt = format!("download-op:{attempt}");
    PluginOperationId::from_uuid(Uuid::new_v5(&plugin_op_id.as_uuid(), salt.as_bytes()))
}

fn build_plugin_download_plan(
    ctx: &PluginEffectContext<'_>,
    payload: DownloaderAddPayload,
    downloader_id: DownloaderId,
    source: TorrentSource,
) -> (FrozenDownloadPlan, DownloadOpContext, Option<i64>) {
    let dl_op_id = derived_download_op_id(ctx.operation_id, ctx.attempt);
    let parsed_site_id = payload
        .site_id
        .as_deref()
        .and_then(|s| SiteId::from_str(s.trim()).ok())
        .unwrap_or_else(|| SiteId::from_uuid(Uuid::nil()));
    let is_boost = ctx.plugin_id == Some(crate::plugins::boost::BUILTIN_BOOST_PLUGIN_ID)
        || !parsed_site_id.as_uuid().is_nil();
    let has_budget_policy =
        is_boost || payload.budget_bytes.is_some() || payload.advertised_size_bytes.is_some();
    let title = payload
        .title
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| format!("plugin:{}", ctx.plugin_id.unwrap_or("unknown")));
    let torrent_id = payload
        .torrent_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let torrent = Torrent {
        site_id: parsed_site_id,
        id: torrent_id,
        title,
        enclosure: payload.download_ref,
        size_bytes: payload.advertised_size_bytes,
        seeders: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        poster_url: None,
        free: false,
        hr: Some(payload.hr.unwrap_or(false)),
        imdb_id: None,
        detail_url: None,
        category: payload.category.clone(),
    };
    let budget_key = has_budget_policy.then(|| {
        format!(
            "{}:{downloader_id}",
            ctx.plugin_id
                .unwrap_or(crate::plugins::boost::BUILTIN_BOOST_PLUGIN_ID)
        )
    });
    let plan = FrozenDownloadPlan {
        torrent,
        source,
        purpose: if is_boost {
            SubmissionPurpose::Boost
        } else {
            SubmissionPurpose::Plugin
        },
        downloader_id: Some(downloader_id),
        frozen_endpoint: None,
        subscribe_id: None,
        subscribe_generation: None,
        policy_revision: None,
        media_id: None,
        score: None,
        release_override: None,
        force: false,
        save_path: payload.relative_directory,
        category: payload.category,
        tags: payload.tags,
        paused: payload.paused,
        upload_limit: payload.upload_limit,
        download_limit: payload.download_limit,
        sequential: payload.sequential,
        first_last_piece: payload.first_last_piece,
        budget_key,
        plan_digest: None,
    };
    let op_ctx = DownloadOpContext {
        operation_id: dl_op_id,
        cause_id: ctx.cause.cause_id.clone(),
        subject: ctx.plugin_id.unwrap_or("plugin").to_string(),
        idempotency_key: format!("plugin-cmd:{}:{}", ctx.operation_id, ctx.attempt),
        plugin_id: ctx.plugin_id.map(str::to_string),
        generation: ctx.generation.and_then(|g| u64::try_from(g).ok()),
        config_revision: None,
        dry_run: false,
    };
    (plan, op_ctx, payload.budget_bytes)
}

fn map_rejected_download_phase(reason: &str) -> String {
    match reason.trim() {
        "active boost reservation already exists for info_hash" => {
            "duplicate_active_reservation".into()
        }
        "boost quota exceeded" => "quota_exceeded".into(),
        other => sanitize_phase_token(other, "rejected"),
    }
}

fn map_download_outcome(
    plugin_op_id: PluginOperationId,
    res: Result<DownloadOutcome, downloader::DownloadSubmitError>,
) -> ExternalEffectStep {
    match res {
        Ok(DownloadOutcome::Submitted {
            operation_id,
            downloader_id,
            info_hash,
            verified_total_bytes,
            already_owned,
            ..
        }) => ExternalEffectStep::Succeeded {
            result: serde_json::to_vec(&serde_json::json!({
                "download_operation_id": operation_id.to_string(),
                "downloader_id": downloader_id.map(|id| id.to_string()),
                "info_hash": info_hash,
                "verified_total_bytes": verified_total_bytes,
                "already_owned": already_owned,
            }))
            .ok(),
        },
        Ok(DownloadOutcome::Rejected { reason, .. }) => {
            let phase = map_rejected_download_phase(&reason);
            tracing::info!(operation_id = %plugin_op_id, %reason, %phase, "plugin downloader.add rejected");
            ExternalEffectStep::Rejected { phase }
        }
        Ok(DownloadOutcome::Deferred {
            until_unix, reason, ..
        }) => {
            tracing::debug!(operation_id = %plugin_op_id, %reason, until_unix, "plugin downloader.add deferred");
            ExternalEffectStep::Deferred {
                next_run_at: until_unix,
                phase: "download-before-defer".into(),
            }
        }
        Ok(DownloadOutcome::Unknown { reason, .. }) => {
            tracing::warn!(operation_id = %plugin_op_id, %reason, "plugin downloader.add outcome unknown");
            ExternalEffectStep::Unknown {
                phase: "downloader-unknown".into(),
            }
        }
        Err(err) => {
            tracing::warn!(operation_id = %plugin_op_id, error = %err, "plugin downloader.add failed");
            ExternalEffectStep::Failed {
                phase: "download-submit-failed".into(),
            }
        }
    }
}

fn ensure_downloader_enabled_for_add(
    store: &store::Store,
    dl_id: DownloaderId,
) -> Result<(), &'static str> {
    let all_dls = store
        .list_downloaders()
        .map_err(|_| "downloader-store-error")?;
    if let Some(row) = all_dls.iter().find(|r| r.id == dl_id) {
        if !row.enabled {
            return Err("downloader-disabled");
        }
    } else if !dl_id.as_uuid().is_nil() {
        return Err("downloader-missing");
    } else if dl_id.as_uuid().is_nil() && all_dls.iter().any(|r| r.is_default && !r.enabled) {
        return Err("downloader-disabled");
    }
    Ok(())
}

pub(crate) fn validate_admin_downloader_add(
    store: &store::Store,
    plugin_id: &str,
    targets: &[PluginCommandTarget],
    payload: &Value,
) -> Result<(), PluginServiceError> {
    let parsed: DownloaderAddPayload = serde_json::from_value(payload.clone())
        .map_err(|e| PluginServiceError::invalid("plugin.invalid_input", e.to_string()))?;
    let dl_id = DownloaderId::from_str(parsed.downloader_id.trim()).map_err(|_| {
        PluginServiceError::invalid("plugin.invalid_input", "invalid downloader_id")
    })?;
    let parsed_site_id =
        match parsed
            .site_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(raw_site) => Some(raw_site.parse::<SiteId>().map_err(|_| {
                PluginServiceError::invalid("plugin.invalid_input", "invalid site_id")
            })?),
            None => None,
        };
    if parsed.download_ref.trim().is_empty() {
        return Err(PluginServiceError::invalid(
            "plugin.invalid_input",
            "download_ref must not be empty",
        ));
    }
    if parsed.budget_bytes.is_some_and(|b| b < 0) {
        return Err(PluginServiceError::invalid(
            "plugin.invalid_input",
            "budget_bytes must be non-negative",
        ));
    }
    if !targets_cover_downloader(targets, dl_id) {
        return Err(PluginServiceError::forbidden(
            "plugin.forbidden",
            "command targets do not cover payload downloader_id",
        ));
    }
    match ensure_downloader_enabled_for_add(store, dl_id) {
        Ok(()) => {}
        Err("downloader-disabled") => {
            return Err(PluginServiceError::Conflict {
                code: "plugin.downloader_disabled",
                message: "target downloader is disabled".into(),
            });
        }
        Err("downloader-missing") => {
            return Err(PluginServiceError::NotFound {
                code: "downloader.missing",
                message: "target downloader does not exist".into(),
            });
        }
        Err(_) => {
            return Err(PluginServiceError::Busy {
                code: "store.busy",
                message: "failed to inspect downloaders".into(),
            });
        }
    }
    if let Some(sid) = parsed_site_id.filter(|s| !s.as_uuid().is_nil()) {
        if !plugin_grants_cover_download_site(store, plugin_id, sid)? {
            return Err(PluginServiceError::forbidden(
                "plugin.forbidden",
                "plugin lacks site grant covering payload site_id",
            ));
        }
        let Some(obs) = store.observe_site(sid)? else {
            return Err(PluginServiceError::not_found(
                "site.missing",
                "target site does not exist",
            ));
        };
        if !obs.site().enabled {
            return Err(PluginServiceError::conflict(
                "plugin.site_disabled",
                "target site is disabled",
            ));
        }
    }
    Ok(())
}

pub(crate) fn execute_plugin_command_step(
    state: &ApiState,
    ctx: &PluginEffectContext<'_>,
) -> ExternalEffectStep {
    if ctx.kind != "downloader.add" {
        return super::plugin_command_ops::execute_downloader_command_step(state, ctx);
    }
    let Some(raw) = ctx.payload else {
        return ExternalEffectStep::Rejected {
            phase: "missing-payload".into(),
        };
    };
    let Ok(payload) = serde_json::from_slice::<DownloaderAddPayload>(raw) else {
        return ExternalEffectStep::Rejected {
            phase: "invalid-payload".into(),
        };
    };
    let Ok(downloader_id) = DownloaderId::from_str(payload.downloader_id.trim()) else {
        return ExternalEffectStep::Rejected {
            phase: "invalid-downloader".into(),
        };
    };
    if !grant_scopes_cover_downloader(ctx.targets, downloader_id) {
        return ExternalEffectStep::Rejected {
            phase: "target-mismatch".into(),
        };
    }
    if let Err(phase) = ensure_downloader_enabled_for_add(&state.store, downloader_id) {
        return ExternalEffectStep::Rejected {
            phase: phase.into(),
        };
    }
    let parsed_site_id = match payload
        .site_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(raw_site) => match raw_site.parse::<SiteId>() {
            Ok(sid) => Some(sid),
            Err(_) => {
                return ExternalEffectStep::Rejected {
                    phase: "invalid-site-id".into(),
                };
            }
        },
        None => None,
    };
    if let Some(sid) = parsed_site_id.filter(|s| !s.as_uuid().is_nil()) {
        let authorized = ctx
            .plugin_id
            .and_then(|pid| plugin_grants_cover_download_site(&state.store, pid, sid).ok())
            .unwrap_or(false);
        if !authorized {
            return ExternalEffectStep::Rejected {
                phase: "site-unauthorized".into(),
            };
        }
        match state.store.observe_site(sid) {
            Ok(Some(obs)) if !obs.site().enabled => {
                return ExternalEffectStep::Rejected {
                    phase: "site-disabled".into(),
                };
            }
            Ok(None) => {
                return ExternalEffectStep::Rejected {
                    phase: "site-missing".into(),
                };
            }
            Err(_) => {
                return ExternalEffectStep::Rejected {
                    phase: "site-store-error".into(),
                };
            }
            Ok(Some(_)) => {}
        }
    }
    let has_site = parsed_site_id.is_some();
    let parse_res = if has_site {
        TorrentSource::from_site_enclosure(payload.download_ref.trim())
    } else {
        TorrentSource::from_url(payload.download_ref.trim())
    };
    let Ok(source) = parse_res else {
        return ExternalEffectStep::Rejected {
            phase: "invalid-download-ref".into(),
        };
    };
    let (plan, op_ctx, budget_cap) =
        build_plugin_download_plan(ctx, payload, downloader_id, source);
    let res = ApiDownloadService::new(state.clone())
        .submit_sync_with_budget_cap(plan, op_ctx, budget_cap);
    map_download_outcome(ctx.operation_id, res)
}

pub(crate) fn reconcile_plugin_command_step(
    state: &ApiState,
    plugin_op_id: PluginOperationId,
    kind: &str,
    attempt: i64,
    targets: &[store::plugins::PluginGrantScope],
) -> Option<PluginReconciliationOutcome> {
    let dl_op_id = derived_download_op_id(plugin_op_id, attempt);
    if let Some(rec) = reconcile_download_operation_by_id(
        state,
        &dl_op_id.to_string(),
        crate::job_loop::unix_now(),
    ) {
        return match rec.state {
            DownloadOperationState::Submitted => Some(PluginReconciliationOutcome::Succeeded),
            DownloadOperationState::Rejected | DownloadOperationState::Failed => {
                Some(PluginReconciliationOutcome::Failed)
            }
            _ => None,
        };
    }
    if kind != "downloader.delete-files"
        && crate::plugins::command_targets::downloader_scope_ids(targets).any(|dl_id| {
            !dl_id.as_uuid().is_nil() && matches!(state.store.get_downloader(dl_id), Ok(None))
        })
    {
        return Some(PluginReconciliationOutcome::Failed);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_valid_phase(phase: &str) -> bool {
        !phase.is_empty()
            && phase.len() <= 128
            && phase
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_:./".contains(&b))
    }

    #[test]
    fn map_download_outcome_sanitizes_rejected_reasons_to_valid_phase() {
        let op_id = PluginOperationId::new();
        for (raw, expected) in [
            (
                "active boost reservation already exists for info_hash",
                Some("duplicate_active_reservation"),
            ),
            ("boost quota exceeded", Some("quota_exceeded")),
            (
                "boost quota budget exhausted",
                Some("boost-quota-budget-exhausted"),
            ),
            (
                "download.before hook rejected: bad tag! (rule #42)",
                Some("download.before-hook-rejected:-bad-tag-rule-42"),
            ),
            ("中文", Some("rejected")),
            ("", Some("rejected")),
        ] {
            let step = map_download_outcome(
                op_id,
                Ok(DownloadOutcome::Rejected {
                    operation_id: op_id,
                    reason: raw.into(),
                    broken_task_hash: None,
                }),
            );
            let ExternalEffectStep::Rejected { phase } = step else {
                panic!("expected Rejected for {raw}");
            };
            assert!(is_valid_phase(&phase), "invalid phase {phase:?} for {raw}");
            if let Some(exp) = expected {
                assert_eq!(phase, exp, "raw={raw}");
            }
        }
        let long = "x ".repeat(120);
        let ExternalEffectStep::Rejected { phase } = map_download_outcome(
            op_id,
            Ok(DownloadOutcome::Rejected {
                operation_id: op_id,
                reason: long,
                broken_task_hash: None,
            }),
        ) else {
            panic!("expected Rejected");
        };
        assert!(is_valid_phase(&phase), "long phase invalid: {phase:?}");
    }

    #[test]
    fn build_plan_preserves_optional_torrent_id_hr_and_validates_site_id() {
        let dl_id = DownloaderId::new();
        let site_id = SiteId::new();
        let subject = store::plugins::PluginSubject::Task {
            plugin_id: "builtin.boost".into(),
            task_id: "traffic-cycle".into(),
        };
        let cause = store::plugins::PluginCommandCause::root("test-cmd");
        let ctx = PluginEffectContext {
            operation_id: PluginOperationId::new(),
            plugin_id: Some("builtin.boost"),
            kind: "downloader.add",
            attempt: 1,
            generation: Some(1),
            subject: &subject,
            canonical_method: "POST /plugins/builtin.boost/commands",
            targets: &[],
            cause: &cause,
            payload: None,
            now: 1_700_000_000,
        };
        let default_payload: DownloaderAddPayload = serde_json::from_value(serde_json::json!({
            "downloader_id": dl_id.to_string(),
            "site_id": site_id.to_string(),
            "download_url": "https://hdarea.club/download.php?id=54321"
        }))
        .unwrap();
        let src = TorrentSource::from_site_enclosure(&default_payload.download_ref).unwrap();
        let (plan_default, _, _) = build_plugin_download_plan(&ctx, default_payload, dl_id, src);
        assert_eq!(plan_default.torrent.id, None);
        assert_eq!(plan_default.torrent.hr, Some(false));

        let explicit_payload: DownloaderAddPayload = serde_json::from_value(serde_json::json!({
            "downloader_id": dl_id.to_string(),
            "site_id": site_id.to_string(),
            "torrent_id": "98765",
            "hr": true,
            "download_url": "https://api.m-team.cc/api/torrent/download?id=98765"
        }))
        .unwrap();
        let src2 = TorrentSource::from_site_enclosure(&explicit_payload.download_ref).unwrap();
        let (plan_explicit, _, _) = build_plugin_download_plan(&ctx, explicit_payload, dl_id, src2);
        assert_eq!(plan_explicit.torrent.id.as_deref(), Some("98765"));
        assert_eq!(plan_explicit.torrent.hr, Some(true));

        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let store = store::Store::open(dir.path().join("db")).unwrap();
        let targets = vec![PluginCommandTarget::Downloader(dl_id)];
        let bad = validate_admin_downloader_add(
            &store,
            "builtin.boost",
            &targets,
            &serde_json::json!({
                "downloader_id": dl_id.to_string(),
                "site_id": "not-a-valid-uuid",
                "download_url": "https://hdarea.club/download.php?id=1"
            }),
        );
        assert!(bad.is_err());
    }
}
