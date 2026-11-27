use std::collections::{HashMap, HashSet};

use domain::SiteId;
use net_guard::Origin;
use plugin_api::GuestRole;
use plugin_host::{
    ENDPOINT_WORLD, HOOK_WORLD, INDEXER_WORLD, Invocation, MAINTENANCE_WORLD, TASK_WORLD,
};
use plugin_package::PluginManifest;
use store::plugins::{PluginGrant, PluginVersionMetadata};

const CAPTURED_COOKIE_TTL: std::time::Duration = std::time::Duration::from_secs(600);
const CAPTURED_COOKIE_MAX_ENTRIES: usize = 1024;

#[derive(Clone, Debug)]
pub(crate) struct CapturedCookieRecord {
    pub site_id: SiteId,
    pub credential_epoch: String,
    pub origin: Origin,
    pub set_cookies: Vec<String>,
    pub merged_cookie: Option<String>,
}

struct StoredCapturedCookie {
    record: CapturedCookieRecord,
    created_at: std::time::Instant,
    seq: u64,
}

#[derive(Default)]
pub(super) struct CapturedCookieBook {
    next_seq: u64,
    entries: HashMap<String, StoredCapturedCookie>,
}

impl CapturedCookieBook {
    pub(super) fn record(&mut self, record: CapturedCookieRecord) -> String {
        let token = format!("http-cookie-{}", uuid::Uuid::new_v4().simple());
        let now = std::time::Instant::now();
        self.entries
            .retain(|_, v| now.saturating_duration_since(v.created_at) <= CAPTURED_COOKIE_TTL);
        while self.entries.len() >= CAPTURED_COOKIE_MAX_ENTRIES {
            let Some(oldest_key) = self
                .entries
                .iter()
                .min_by_key(|(_, v)| (v.created_at, v.seq))
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.entries.remove(&oldest_key);
        }
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        self.entries.insert(
            token.clone(),
            StoredCapturedCookie {
                record,
                created_at: now,
                seq,
            },
        );
        token
    }

    pub(super) fn take(&mut self, token: &str) -> Option<CapturedCookieRecord> {
        let now = std::time::Instant::now();
        self.entries
            .retain(|_, v| now.saturating_duration_since(v.created_at) <= CAPTURED_COOKIE_TTL);
        self.entries.remove(token).map(|entry| entry.record)
    }
}

pub(super) fn filter_grants_for_manifest_invocation(
    plugin_id: &str,
    version: &PluginVersionMetadata,
    inv: &Invocation,
    grants: &mut Vec<PluginGrant>,
) {
    if version.manifest_json == "{}" {
        return;
    }
    let Ok(manifest) = serde_json::from_str::<PluginManifest>(&version.manifest_json) else {
        grants.clear();
        return;
    };
    if manifest.components.is_empty() {
        return;
    }
    let target_world = match inv.role {
        GuestRole::Filter | GuestRole::Gate | GuestRole::Notify => HOOK_WORLD,
        GuestRole::Task => TASK_WORLD,
        GuestRole::Endpoint => ENDPOINT_WORLD,
        GuestRole::Indexer => INDEXER_WORLD,
        GuestRole::Maintenance => MAINTENANCE_WORLD,
        GuestRole::Lifecycle => return,
    };
    let mut allowed = HashSet::new();
    match inv.provider_id.as_deref() {
        Some(pid) => {
            let world_comps: Vec<_> = manifest
                .components
                .iter()
                .filter(|c| c.world == target_world)
                .collect();
            let sub_matched: Vec<_> = match inv.role {
                GuestRole::Indexer => world_comps
                    .iter()
                    .copied()
                    .filter(|c| {
                        c.indexers.iter().any(|idx| {
                            idx.id == pid || format!("plugin:{plugin_id}/{}", idx.id) == pid
                        })
                    })
                    .collect(),
                GuestRole::Maintenance => world_comps
                    .iter()
                    .copied()
                    .filter(|c| {
                        c.maintenance
                            .iter()
                            .any(|m| m.id == pid || format!("plugin:{plugin_id}/{}", m.id) == pid)
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let matched = if !sub_matched.is_empty() {
                sub_matched
            } else {
                world_comps.into_iter().filter(|c| c.id == pid).collect()
            };
            if let [only] = matched.as_slice() {
                allowed.extend(only.permissions.iter().cloned());
            }
        }
        None => {
            let world_comps: Vec<_> = manifest
                .components
                .iter()
                .filter(|c| c.world == target_world)
                .collect();
            if let [only] = world_comps.as_slice() {
                allowed.extend(only.permissions.iter().cloned());
            }
        }
    }
    grants.retain(|g| allowed.contains(&g.capability));
}
