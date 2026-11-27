use domain::SiteId;
use hooks::HookPipeline;
use indexer::MaintenanceAction;
use serde::Deserialize;

use super::{MaintenanceError, execute_receipt_maintenance, perform_login};
use crate::management::ApiState;
use crate::plugins::{ExternalEffectStep, PluginEffectContext, PluginServiceError};

#[derive(Deserialize)]
struct SiteMaintenanceCommandPayload {
    site_id: SiteId,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    provider_id: Option<String>,
    #[serde(default)]
    expected_site_revision: Option<i64>,
    #[serde(default)]
    expected_credential_epoch: Option<String>,
}

fn map_maintenance_error_step(
    site_id: SiteId,
    operation_id: domain::PluginOperationId,
    action: MaintenanceAction,
    err: MaintenanceError,
) -> ExternalEffectStep {
    match err {
        MaintenanceError::Unknown(msg) => {
            let msg = hooks::sanitize_reason(&msg);
            tracing::warn!(
                %site_id,
                %operation_id,
                action = action.as_str(),
                error = %msg,
                "Site maintenance command outcome unknown"
            );
            ExternalEffectStep::Unknown {
                phase: "unknown-outcome".into(),
            }
        }
        MaintenanceError::BlockedUnknown(msg) => {
            let msg = hooks::sanitize_reason(&msg);
            tracing::warn!(
                %site_id,
                %operation_id,
                action = action.as_str(),
                error = %msg,
                "Site maintenance command blocked by unknown receipt"
            );
            ExternalEffectStep::Unknown {
                phase: "blocked-unknown".into(),
            }
        }
        MaintenanceError::Unsupported(msg) => {
            let msg = hooks::sanitize_reason(&msg);
            tracing::warn!(
                %site_id,
                %operation_id,
                action = action.as_str(),
                error = %msg,
                "Site maintenance command unsupported"
            );
            ExternalEffectStep::Rejected {
                phase: "unsupported".into(),
            }
        }
        MaintenanceError::InvalidTimezone(msg) => {
            let msg = hooks::sanitize_reason(&msg);
            tracing::warn!(
                %site_id,
                %operation_id,
                action = action.as_str(),
                error = %msg,
                "Site maintenance command rejected due to invalid timezone"
            );
            ExternalEffectStep::Rejected {
                phase: "invalid-timezone".into(),
            }
        }
        MaintenanceError::InFlight => {
            tracing::warn!(
                %site_id,
                %operation_id,
                action = action.as_str(),
                "Site maintenance command failed because another attempt is in flight"
            );
            ExternalEffectStep::Failed {
                phase: "in-flight".into(),
            }
        }
        MaintenanceError::Failed(msg) => {
            let msg = hooks::sanitize_reason(&msg);
            tracing::warn!(
                %site_id,
                %operation_id,
                action = action.as_str(),
                error = %msg,
                "Site maintenance command failed"
            );
            ExternalEffectStep::Failed {
                phase: "maintenance-failed".into(),
            }
        }
    }
}

pub(crate) fn parse_payload_site_id(
    payload: &serde_json::Value,
) -> Result<SiteId, PluginServiceError> {
    let parsed: SiteMaintenanceCommandPayload = serde_json::from_value(payload.clone())
        .map_err(|e| PluginServiceError::invalid("plugin.invalid_input", e.to_string()))?;
    Ok(parsed.site_id)
}

fn execute_login_effect_step(
    state: &ApiState,
    ctx: &PluginEffectContext<'_>,
    req: &SiteMaintenanceCommandPayload,
    observed: &store::SiteObservation,
) -> ExternalEffectStep {
    let resolved = super::resolve_maintenance_provider(
        state,
        observed.site(),
        MaintenanceAction::VerifySession,
        req.provider_id.as_deref(),
    );
    let (plugin_id, _, _) = match resolved {
        Ok(triple) => triple,
        Err(err) => {
            return map_maintenance_error_step(
                req.site_id,
                ctx.operation_id,
                MaintenanceAction::VerifySession,
                err,
            );
        }
    };
    let use_provider = req.provider_id.is_some()
        || plugin_id != crate::plugins::builtins::BUILTIN_NEXUSPHP_MAINTENANCE_PLUGIN_ID
        || state
            .indexer
            .snapshot()
            .profile(&observed.site().profile_id)
            .is_none();
    if use_provider {
        return match execute_receipt_maintenance(
            state,
            observed,
            MaintenanceAction::VerifySession,
            req.provider_id.as_deref(),
            ctx.now,
            &ctx.operation_id.to_string(),
        ) {
            Ok(res) => ExternalEffectStep::Succeeded {
                result: serde_json::to_vec(&serde_json::json!({
                    "site_id": req.site_id,
                    "action": "login",
                    "ok": true,
                    "receipt_id": res.receipt.receipt_id,
                    "state": res.receipt.state.as_str(),
                    "deduplicated": res.deduplicated,
                }))
                .ok(),
            },
            Err(err) => map_maintenance_error_step(
                req.site_id,
                ctx.operation_id,
                MaintenanceAction::VerifySession,
                err,
            ),
        };
    }
    let bus = HookPipeline::new();
    let creds = crate::check_in::StoreSites::new(state, observed.clone());
    match perform_login(state, observed.site(), &bus, &creds) {
        Ok(()) => ExternalEffectStep::Succeeded {
            result: serde_json::to_vec(&serde_json::json!({
                "site_id": req.site_id,
                "action": "login",
                "ok": true,
            }))
            .ok(),
        },
        Err(err) => {
            let clean = hooks::sanitize_reason(&err.to_string());
            tracing::warn!(
                site_id = %req.site_id,
                operation_id = %ctx.operation_id,
                error = %clean,
                "Site login command failed"
            );
            ExternalEffectStep::Failed {
                phase: "login-failed".into(),
            }
        }
    }
}

pub(crate) fn execute_effect_step(
    state: &ApiState,
    ctx: &PluginEffectContext<'_>,
) -> ExternalEffectStep {
    let Some(raw) = ctx.payload else {
        return ExternalEffectStep::Rejected {
            phase: "missing-payload".into(),
        };
    };
    let Ok(req) = serde_json::from_slice::<SiteMaintenanceCommandPayload>(raw) else {
        return ExternalEffectStep::Rejected {
            phase: "invalid-payload".into(),
        };
    };
    if !crate::plugins::command_targets::grant_scopes_cover_site(ctx.targets, req.site_id) {
        return ExternalEffectStep::Rejected {
            phase: "target-mismatch".into(),
        };
    }
    let action_str = match ctx.kind {
        "site.check-in" => "check-in",
        "site.verify-session" => "verify-session",
        _ => req.action.as_deref().unwrap_or("check-in"),
    };
    let Some(observed) = state.store.observe_site(req.site_id).ok().flatten() else {
        return ExternalEffectStep::Failed {
            phase: "site-missing".into(),
        };
    };
    if req
        .expected_site_revision
        .is_some_and(|rev| observed.revision() != rev)
        || req
            .expected_credential_epoch
            .as_deref()
            .is_some_and(|epoch| observed.credential_epoch().storage_key() != epoch)
    {
        tracing::warn!(
            site_id = %req.site_id,
            operation_id = %ctx.operation_id,
            "Site maintenance command rejected because site revision or credentials changed"
        );
        return ExternalEffectStep::Rejected {
            phase: "site-changed".into(),
        };
    }

    if matches!(action_str, "login") {
        return execute_login_effect_step(state, ctx, &req, &observed);
    }

    let action = match action_str {
        "verify" | "verify-session" => MaintenanceAction::VerifySession,
        "checkin" | "check-in" => MaintenanceAction::CheckIn,
        _ => {
            return ExternalEffectStep::Rejected {
                phase: "invalid-action".into(),
            };
        }
    };

    let outcome = if action == MaintenanceAction::CheckIn && req.provider_id.is_none() {
        match super::try_perform_api_check_in(state, &observed) {
            Some(res) => res,
            None => execute_receipt_maintenance(
                state,
                &observed,
                action,
                None,
                ctx.now,
                &ctx.operation_id.to_string(),
            ),
        }
    } else {
        execute_receipt_maintenance(
            state,
            &observed,
            action,
            req.provider_id.as_deref(),
            ctx.now,
            &ctx.operation_id.to_string(),
        )
    };

    match outcome {
        Ok(res) => ExternalEffectStep::Succeeded {
            result: serde_json::to_vec(&serde_json::json!({
                "receipt_id": res.receipt.receipt_id,
                "action": action.as_str(),
                "state": res.receipt.state.as_str(),
                "deduplicated": res.deduplicated,
            }))
            .ok(),
        },
        Err(err) => map_maintenance_error_step(req.site_id, ctx.operation_id, action, err),
    }
}
