use std::sync::Arc;

use hooks::{Gate, PluginInvoker};
use plugin_api::{FilterPayload, GateInput, GatePayload};
use store::plugins::PluginRecord;

use super::config::BuiltinBoostConfig;
use super::snapshot::{SnapshotQuery, collect_boost_snapshot_sync};
use crate::management::ApiState;

pub(crate) struct BuiltinBoostDownloadGate {
    state: ApiState,
    plugin: PluginRecord,
}

impl BuiltinBoostDownloadGate {
    pub(crate) fn new(state: ApiState, plugin: PluginRecord) -> Self {
        Self { state, plugin }
    }

    fn load_effective_config(&self) -> BuiltinBoostConfig {
        let cfg_val = self
            .state
            .store
            .get_plugin_config(&self.plugin.plugin_id)
            .ok()
            .flatten()
            .map(|c| c.values)
            .unwrap_or_else(|| serde_json::json!({}));
        BuiltinBoostConfig::from_layers(&[&cfg_val]).unwrap_or_default()
    }
}

impl PluginInvoker for BuiltinBoostDownloadGate {
    fn gate_typed(&self, input: &GateInput) -> Result<Gate, String> {
        let GatePayload::DownloadBefore(before) = &input.payload else {
            return Ok(Gate::Abstain);
        };
        if self
            .state
            .store
            .issue_plugin_state_authority(&self.plugin, None)
            .is_err()
        {
            return Ok(Gate::Abstain);
        }
        let Ok(grants) = self.state.store.get_plugin_grants(&self.plugin.plugin_id) else {
            return Ok(Gate::Abstain);
        };
        if !crate::plugins::hooks_runtime::hook_grants_cover_context(
            &grants,
            "download.before",
            &input.context,
        ) {
            return Ok(Gate::Abstain);
        }
        let cfg = self.load_effective_config();
        let is_boost = before.purpose == "boost";
        if !is_boost && (!cfg.enforce_budget_on_all || cfg.is_unconfigured()) {
            return Ok(Gate::Abstain);
        }

        let dl_filter: Vec<domain::DownloaderId> = before
            .plan
            .downloader_id
            .parse::<domain::DownloaderId>()
            .ok()
            .into_iter()
            .collect();
        let snap = match collect_boost_snapshot_sync(
            &self.state.store,
            &self.state.downloader,
            &SnapshotQuery {
                plugin_id: &self.plugin.plugin_id,
                site_ids: &[],
                downloader_ids: &dl_filter,
                allowed_sites: None,
                allowed_downloaders: None,
            },
        ) {
            Ok(s) => s,
            Err(err) => {
                if is_boost {
                    return Ok(Gate::Defer {
                        until_ms: 60_000,
                        reason: format!("boost snapshot unavailable: {err}"),
                    });
                }
                return Ok(Gate::Abstain);
            }
        };

        if let Some(budget) = cfg.budget_bytes {
            let occupied = snap
                .used_bytes
                .saturating_add(snap.reserved_bytes)
                .saturating_add(snap.retained_file_bytes);
            if occupied >= u64::try_from(budget).unwrap_or(0) {
                return Ok(Gate::Reject("boost quota budget exhausted".into()));
            }
        }

        if cfg.min_free_disk_bytes > 0 {
            let min_free = u64::try_from(cfg.min_free_disk_bytes).unwrap_or(0);
            if snap.disk.status != "available"
                || snap.disk.available_bytes.is_none_or(|b| b < min_free)
            {
                return Ok(Gate::Defer {
                    until_ms: 60_000,
                    reason: "boost disk space unavailable or below threshold".into(),
                });
            }
        }

        Ok(Gate::Allow)
    }
}

#[allow(dead_code)]
fn _unused_filter_payload_ref(_: &FilterPayload, _: &Arc<ApiState>) {}
