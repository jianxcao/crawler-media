use std::collections::HashSet;

use domain::{DownloaderId, SiteId};
use serde_json::Value;

#[derive(Clone, Debug)]
pub(crate) struct BuiltinBoostConfig {
    pub sites: Vec<SiteId>,
    pub downloader_id: Option<DownloaderId>,
    pub budget_bytes: Option<i64>,
    pub min_free_disk_bytes: i64,
    pub hold_days: i64,
    pub max_active: usize,
    pub max_adds_per_hour: i64,
    pub max_adds_per_round: usize,
    pub dry_run: bool,
    pub auto_cleanup_files: bool,
    pub min_seeders: i32,
    pub free_only: bool,
    pub exclude_hr: bool,
    pub enforce_budget_on_all: bool,
}

impl Default for BuiltinBoostConfig {
    fn default() -> Self {
        Self {
            sites: Vec::new(),
            downloader_id: None,
            budget_bytes: None,
            min_free_disk_bytes: 0,
            hold_days: 0,
            max_active: 20,
            max_adds_per_hour: 10,
            max_adds_per_round: 20,
            dry_run: true,
            auto_cleanup_files: false,
            min_seeders: 1,
            free_only: true,
            exclude_hr: true,
            enforce_budget_on_all: false,
        }
    }
}

impl BuiltinBoostConfig {
    pub(crate) fn from_layers(layers: &[&Value]) -> Result<Self, String> {
        let mut cfg = Self::default();
        for layer in layers {
            let Some(obj) = layer.as_object() else {
                continue;
            };
            cfg.apply_object(obj)?;
        }
        Ok(cfg)
    }

    fn apply_object(&mut self, obj: &serde_json::Map<String, Value>) -> Result<(), String> {
        if let Some(arr) = obj.get("sites").and_then(|v| v.as_array()) {
            let mut seen = HashSet::new();
            let mut parsed = Vec::new();
            for item in arr {
                let Some(s) = item.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
                    continue;
                };
                let sid: SiteId = s.parse().map_err(|_| format!("invalid site_id `{s}`"))?;
                if seen.insert(sid) {
                    parsed.push(sid);
                }
            }
            self.sites = parsed;
        }
        if let Some(s) = obj
            .get("site_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let sid: SiteId = s.parse().map_err(|_| format!("invalid site_id `{s}`"))?;
            if !self.sites.contains(&sid) {
                self.sites.insert(0, sid);
            }
        }
        if let Some(raw) = obj.get("downloader_id").and_then(|v| v.as_str()) {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                self.downloader_id = None;
            } else {
                let dl_id: DownloaderId = trimmed
                    .parse()
                    .map_err(|_| format!("invalid downloader_id `{trimmed}`"))?;
                self.downloader_id = Some(dl_id);
            }
        }
        if let Some(v) = obj.get("budget_bytes").and_then(|v| v.as_i64())
            && v >= 0
        {
            self.budget_bytes = Some(v);
        }
        if let Some(v) = obj.get("min_free_disk_bytes").and_then(|v| v.as_i64())
            && v >= 0
        {
            self.min_free_disk_bytes = v;
        }
        if let Some(v) = obj.get("hold_days").and_then(|v| v.as_i64())
            && v >= 0
        {
            self.hold_days = v;
        }
        if let Some(v) = obj.get("max_active").and_then(|v| v.as_u64()) {
            self.max_active = usize::try_from(v.clamp(1, 200)).unwrap_or(20);
        }
        if let Some(v) = obj.get("max_adds_per_hour").and_then(|v| v.as_i64())
            && v > 0
        {
            self.max_adds_per_hour = v;
        }
        if let Some(v) = obj.get("max_adds_per_round").and_then(|v| v.as_u64()) {
            self.max_adds_per_round = usize::try_from(v.clamp(1, 100)).unwrap_or(20);
        }
        if let Some(v) = obj.get("dry_run").and_then(|v| v.as_bool()) {
            self.dry_run = v;
        }
        if let Some(v) = obj.get("auto_cleanup_files").and_then(|v| v.as_bool()) {
            self.auto_cleanup_files = v;
        }
        if let Some(v) = obj.get("min_seeders").and_then(|v| v.as_i64())
            && v >= 0
        {
            self.min_seeders = i32::try_from(v).unwrap_or(i32::MAX);
        }
        if let Some(v) = obj.get("free_only").and_then(|v| v.as_bool()) {
            self.free_only = v;
        }
        if let Some(v) = obj.get("exclude_hr").and_then(|v| v.as_bool()) {
            self.exclude_hr = v;
        }
        if let Some(v) = obj.get("enforce_budget_on_all").and_then(|v| v.as_bool()) {
            self.enforce_budget_on_all = v;
        }
        Ok(())
    }

    pub(crate) fn quota_key(&self, downloader_id: DownloaderId) -> String {
        format!("{}:{downloader_id}", super::BUILTIN_BOOST_PLUGIN_ID)
    }

    pub(crate) fn is_unconfigured(&self) -> bool {
        self.budget_bytes.is_none() && self.sites.is_empty()
    }
}
