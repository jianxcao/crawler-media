use std::collections::HashSet;
use std::path::{Component, Path};

use domain::{DownloaderId, SiteId};
use regex_automata::meta::{Builder as RegexBuilder, Config as RegexConfig, Regex};
use serde_json::{Map, Value};

const REGEX_SIZE_LIMIT_BYTES: usize = 1 << 20;
const MAX_REGEX_PATTERN_LEN: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum FreeMode {
    Any,
    #[default]
    Free,
    TwoXFree,
}

impl FreeMode {
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "any" => Ok(Self::Any),
            "free" => Ok(Self::Free),
            "2x_free" | "2xfree" => Ok(Self::TwoXFree),
            other => Err(format!("invalid free_mode `{other}`")),
        }
    }

    pub(crate) fn is_free_only(self) -> bool {
        !matches!(self, Self::Any)
    }

    pub(crate) fn matches(self, free: bool, free_2x: bool) -> bool {
        match self {
            Self::Any => true,
            Self::Free => free || free_2x,
            Self::TwoXFree => free && free_2x,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ActiveHours {
    pub start_min: u16,
    pub end_min: u16,
}

impl ActiveHours {
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        let Some((lhs, rhs)) = trimmed.split_once('-') else {
            return Err(format!("invalid active_hours `{trimmed}`"));
        };
        if rhs.contains('-') {
            return Err(format!("invalid active_hours `{trimmed}`"));
        }
        let start_min =
            parse_hhmm(lhs).ok_or_else(|| format!("invalid active_hours `{trimmed}`"))?;
        let end_min = parse_hhmm(rhs).ok_or_else(|| format!("invalid active_hours `{trimmed}`"))?;
        Ok(Self { start_min, end_min })
    }

    pub(crate) fn is_active_at(self, now_utc: i64, site_timezone: Option<&str>) -> bool {
        let tz = site_timezone
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("UTC");
        let offset = i64::from(indexer::resolve_offset_secs(tz, now_utc));
        let local_min = u16::try_from((now_utc + offset).rem_euclid(86_400) / 60).unwrap_or(0);
        if self.start_min == self.end_min {
            true
        } else if self.start_min < self.end_min {
            local_min >= self.start_min && local_min < self.end_min
        } else {
            local_min >= self.start_min || local_min < self.end_min
        }
    }
}

fn parse_hhmm(raw: &str) -> Option<u16> {
    let (hh, mm) = raw.trim().split_once(':')?;
    if hh.len() != 2
        || mm.len() != 2
        || !hh.bytes().all(|b| b.is_ascii_digit())
        || !mm.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let (h, m): (u16, u16) = (hh.parse().ok()?, mm.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

#[derive(Clone, Debug)]
pub(crate) struct CompiledRegex {
    regex: Regex,
}

impl CompiledRegex {
    pub(crate) fn compile(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.len() > MAX_REGEX_PATTERN_LEN {
            return Err("regex pattern exceeds maximum length".into());
        }
        let cfg = RegexConfig::new()
            .nfa_size_limit(Some(REGEX_SIZE_LIMIT_BYTES))
            .dfa_size_limit(Some(REGEX_SIZE_LIMIT_BYTES));
        let regex = RegexBuilder::new()
            .configure(cfg)
            .build(trimmed)
            .map_err(|e| format!("invalid regex `{trimmed}`: {e}"))?;
        Ok(Self { regex })
    }

    pub(crate) fn is_match(&self, haystack: &str) -> bool {
        self.regex.is_match(haystack)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct EffectiveBoostRule {
    pub categories: Vec<String>,
    pub free_mode: FreeMode,
    pub free_only: bool,
    pub exclude_hr: bool,
    pub min_seeders: i32,
    pub max_seeders: Option<i32>,
    pub min_leechers: Option<i32>,
    pub min_size_bytes: Option<u64>,
    pub max_size_bytes: Option<u64>,
    pub max_pub_age_minutes: Option<u64>,
    pub include_regex: Option<CompiledRegex>,
    pub exclude_regex: Option<CompiledRegex>,
    pub active_hours: Option<ActiveHours>,
    pub target_site_ratio: Option<f64>,
    pub hold_days: i64,
    pub max_adds_per_round: usize,
    pub delete_on_free_expired: bool,
    pub max_seed_hours: Option<f64>,
    pub target_task_ratio: Option<f64>,
    pub target_uploaded_bytes: Option<u64>,
    pub max_download_hours: Option<f64>,
    pub min_avg_upspeed_bytes: Option<u64>,
    pub max_inactive_minutes: Option<u64>,
    pub relative_directory: Option<String>,
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub upload_limit_bytes: Option<u64>,
    pub download_limit_bytes: Option<u64>,
    pub sequential: bool,
    pub first_last_piece: bool,
}

impl Default for EffectiveBoostRule {
    fn default() -> Self {
        Self {
            categories: Vec::new(),
            free_mode: FreeMode::Free,
            free_only: true,
            exclude_hr: true,
            min_seeders: 1,
            max_seeders: None,
            min_leechers: None,
            min_size_bytes: None,
            max_size_bytes: None,
            max_pub_age_minutes: None,
            include_regex: None,
            exclude_regex: None,
            active_hours: None,
            target_site_ratio: None,
            hold_days: 0,
            max_adds_per_round: 20,
            delete_on_free_expired: false,
            max_seed_hours: None,
            target_task_ratio: None,
            target_uploaded_bytes: None,
            max_download_hours: None,
            min_avg_upspeed_bytes: None,
            max_inactive_minutes: None,
            relative_directory: None,
            category: None,
            tags: Vec::new(),
            upload_limit_bytes: None,
            download_limit_bytes: None,
            sequential: false,
            first_last_piece: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct BoostRuleOverrides {
    pub categories: Option<Vec<String>>,
    pub free_mode: Option<FreeMode>,
    pub exclude_hr: Option<bool>,
    pub min_seeders: Option<i32>,
    pub max_seeders: Option<Option<i32>>,
    pub min_leechers: Option<Option<i32>>,
    pub min_size_bytes: Option<Option<u64>>,
    pub max_size_bytes: Option<Option<u64>>,
    pub max_pub_age_minutes: Option<Option<u64>>,
    pub include_regex: Option<Option<CompiledRegex>>,
    pub exclude_regex: Option<Option<CompiledRegex>>,
    pub active_hours: Option<Option<ActiveHours>>,
    pub target_site_ratio: Option<Option<f64>>,
    pub hold_days: Option<i64>,
    pub max_adds_per_round: Option<usize>,
    pub delete_on_free_expired: Option<bool>,
    pub max_seed_hours: Option<Option<f64>>,
    pub target_task_ratio: Option<Option<f64>>,
    pub target_uploaded_bytes: Option<Option<u64>>,
    pub max_download_hours: Option<Option<f64>>,
    pub min_avg_upspeed_bytes: Option<Option<u64>>,
    pub max_inactive_minutes: Option<Option<u64>>,
    pub relative_directory: Option<Option<String>>,
    pub category: Option<Option<String>>,
    pub tags: Option<Vec<String>>,
    pub upload_limit_bytes: Option<Option<u64>>,
    pub download_limit_bytes: Option<Option<u64>>,
    pub sequential: Option<bool>,
    pub first_last_piece: Option<bool>,
}

#[derive(Clone, Debug)]
pub(crate) struct BoostRuleConfig {
    pub site_id: SiteId,
    pub enabled: bool,
    pub overrides: BoostRuleOverrides,
}

#[derive(Clone, Debug)]
pub(crate) struct BoostConfig {
    pub sites: Vec<SiteId>,
    pub downloader_id: Option<DownloaderId>,
    pub budget_bytes: Option<i64>,
    pub min_free_disk_bytes: i64,
    pub watermark_low_bytes: Option<i64>,
    pub dynamic_delete: bool,
    pub max_active: usize,
    pub max_downloading: Option<usize>,
    pub max_adds_per_hour: i64,
    pub hold_days: i64,
    pub dry_run: bool,
    pub auto_cleanup_files: bool,
    pub enforce_budget_on_all: bool,
    pub global_max_upspeed_bytes: Option<u64>,
    pub global_max_dlspeed_bytes: Option<u64>,
    pub defaults: EffectiveBoostRule,
    pub rules: Vec<BoostRuleConfig>,
}

impl Default for BoostConfig {
    fn default() -> Self {
        Self {
            sites: Vec::new(),
            downloader_id: None,
            budget_bytes: None,
            min_free_disk_bytes: 0,
            watermark_low_bytes: None,
            dynamic_delete: false,
            max_active: 20,
            max_downloading: None,
            max_adds_per_hour: 10,
            hold_days: 0,
            dry_run: true,
            auto_cleanup_files: false,
            enforce_budget_on_all: false,
            global_max_upspeed_bytes: None,
            global_max_dlspeed_bytes: None,
            defaults: EffectiveBoostRule::default(),
            rules: Vec::new(),
        }
    }
}

pub(crate) fn quota_key(plugin_id: &str, downloader_id: DownloaderId) -> String {
    format!("{plugin_id}:{downloader_id}")
}

macro_rules! assign_opts {
    ($dst:expr, $src:expr; copy: [$($c:ident),*]; clone: [$($l:ident),*]) => {
        $(if let Some(v) = $src.$c { $dst.$c = v; })*
        $(if let Some(v) = &$src.$l { $dst.$l = v.clone(); })*
    };
}

macro_rules! parse_num_fields {
    ($obj:expr, $out:expr; u64: [$($u:ident),*]; f64: [$($f:ident),*]; bool: [$($b:ident),*]) => {
        $(if let Some(v) = opt_u64($obj, stringify!($u))? { $out.$u = Some(v); })*
        $(if let Some(v) = opt_f64($obj, stringify!($f))? { $out.$f = Some(v); })*
        $($out.$b = $obj.get(stringify!($b)).and_then(Value::as_bool);)*
    };
}

impl BoostConfig {
    pub(crate) fn from_layers(layers: &[&Value]) -> Result<Self, String> {
        let mut cfg = Self::default();
        for layer in layers {
            if let Some(obj) = layer.as_object() {
                cfg.apply_object(obj)?;
            }
        }
        Ok(cfg)
    }

    pub(crate) fn rule_for_site(&self, site_id: SiteId) -> Option<&BoostRuleConfig> {
        self.rules.iter().find(|r| r.site_id == site_id)
    }

    pub(crate) fn is_site_enabled(&self, site_id: SiteId) -> bool {
        self.rule_for_site(site_id).is_none_or(|r| r.enabled)
    }

    pub(crate) fn effective_for_site(&self, site_id: Option<SiteId>) -> EffectiveBoostRule {
        let mut effective = self.defaults.clone();
        if let Some(sid) = site_id
            && let Some(rule) = self.rule_for_site(sid)
        {
            effective.apply_overrides(&rule.overrides, true);
        }
        effective
    }

    pub(crate) fn effective_hold_days(&self, site_id: Option<SiteId>) -> i64 {
        site_id
            .and_then(|sid| self.rule_for_site(sid))
            .filter(|r| r.enabled)
            .and_then(|r| r.overrides.hold_days)
            .unwrap_or(self.defaults.hold_days)
    }

    fn apply_object(&mut self, obj: &Map<String, Value>) -> Result<(), String> {
        self.apply_global_scope(obj)?;
        self.apply_global_gates(obj)?;
        let overrides = BoostRuleOverrides::from_object(obj)?;
        self.defaults.apply_overrides(&overrides, false);
        validate_effective_ranges(&self.defaults)?;
        self.hold_days = self.defaults.hold_days;
        if let Some(rules_val) = obj.get("rules") {
            self.rules = parse_rules_array(rules_val, &self.defaults)?;
        }
        Ok(())
    }

    fn apply_global_scope(&mut self, obj: &Map<String, Value>) -> Result<(), String> {
        if let Some(arr) = obj.get("sites").and_then(Value::as_array) {
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
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let sid: SiteId = s.parse().map_err(|_| format!("invalid site_id `{s}`"))?;
            if !self.sites.contains(&sid) {
                self.sites.insert(0, sid);
            }
        }
        if let Some(val) = obj.get("downloader_id") {
            self.downloader_id = match val {
                Value::Null => None,
                Value::String(raw) => {
                    let t = raw.trim();
                    if t.is_empty() {
                        None
                    } else {
                        Some(
                            t.parse()
                                .map_err(|_| format!("invalid downloader_id `{t}`"))?,
                        )
                    }
                }
                _ => return Err("downloader_id must be a string".into()),
            };
        }
        if let Some(v) = opt_i64(obj, "budget_bytes")? {
            self.budget_bytes = v;
        }
        if let Some(Some(v)) = opt_i64(obj, "min_free_disk_bytes")? {
            self.min_free_disk_bytes = v;
        }
        if let Some(v) = opt_i64(obj, "watermark_low_bytes")? {
            self.watermark_low_bytes = v;
        }
        if let Some(v) = obj.get("dynamic_delete").and_then(Value::as_bool) {
            self.dynamic_delete = v;
        }
        Ok(())
    }

    fn apply_global_gates(&mut self, obj: &Map<String, Value>) -> Result<(), String> {
        if let Some(v) = obj.get("max_active").and_then(Value::as_u64) {
            self.max_active = usize::try_from(v.clamp(1, 200)).unwrap_or(20);
        }
        if let Some(opt) = opt_u64(obj, "max_downloading")? {
            if opt == Some(0) {
                return Err("max_downloading must be >= 1".into());
            }
            self.max_downloading = opt.map(|n| usize::try_from(n.clamp(1, 200)).unwrap_or(200));
        }
        if let Some(v) = obj.get("max_adds_per_hour").and_then(Value::as_i64)
            && v > 0
        {
            self.max_adds_per_hour = v;
        }
        for (dst, key) in [
            (&mut self.dry_run, "dry_run"),
            (&mut self.auto_cleanup_files, "auto_cleanup_files"),
            (&mut self.enforce_budget_on_all, "enforce_budget_on_all"),
        ] {
            if let Some(v) = obj.get(key).and_then(Value::as_bool) {
                *dst = v;
            }
        }
        if let Some(v) = opt_u64(obj, "global_max_upspeed_bytes")? {
            self.global_max_upspeed_bytes = v;
        }
        if let Some(v) = opt_u64(obj, "global_max_dlspeed_bytes")? {
            self.global_max_dlspeed_bytes = v;
        }
        Ok(())
    }
}

impl EffectiveBoostRule {
    fn apply_overrides(&mut self, ov: &BoostRuleOverrides, merge_tags: bool) {
        if let Some(fm) = ov.free_mode {
            self.free_mode = fm;
            self.free_only = fm.is_free_only();
        }
        assign_opts!(
            self,
            ov;
            copy: [
                exclude_hr, min_seeders, max_seeders, min_leechers, min_size_bytes,
                max_size_bytes, max_pub_age_minutes, active_hours, target_site_ratio, hold_days,
                max_adds_per_round, delete_on_free_expired, max_seed_hours, target_task_ratio,
                target_uploaded_bytes, max_download_hours, min_avg_upspeed_bytes,
                max_inactive_minutes, upload_limit_bytes, download_limit_bytes, sequential,
                first_last_piece
            ];
            clone: [categories, include_regex, exclude_regex, relative_directory, category]
        );
        if let Some(tags) = &ov.tags {
            if merge_tags {
                for t in tags {
                    if !self.tags.contains(t) {
                        self.tags.push(t.clone());
                    }
                }
            } else {
                self.tags = tags.clone();
            }
        }
    }
}

impl BoostRuleOverrides {
    fn from_object(obj: &Map<String, Value>) -> Result<Self, String> {
        let mut out = Self::default();
        parse_num_fields!(
            obj,
            out;
            u64: [
                min_size_bytes, max_size_bytes, max_pub_age_minutes, target_uploaded_bytes,
                min_avg_upspeed_bytes, max_inactive_minutes
            ];
            f64: [target_site_ratio, max_seed_hours, target_task_ratio, max_download_hours];
            bool: [exclude_hr, delete_on_free_expired, sequential, first_last_piece]
        );
        parse_rule_special_fields(obj, &mut out)?;
        Ok(out)
    }
}

fn parse_rule_special_fields(
    obj: &Map<String, Value>,
    out: &mut BoostRuleOverrides,
) -> Result<(), String> {
    if let Some(val) = obj.get("categories") {
        let arr = val
            .as_array()
            .ok_or_else(|| "categories must be an array".to_string())?;
        let mut cats = Vec::new();
        for item in arr {
            let s = item
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "categories items must be non-empty strings".to_string())?;
            let norm = s.to_ascii_lowercase();
            if !cats.contains(&norm) {
                cats.push(norm);
            }
        }
        out.categories = Some(cats);
    }
    if let Some(val) = obj.get("free_mode").filter(|v| !v.is_null()) {
        let raw = val
            .as_str()
            .ok_or_else(|| "free_mode must be a string".to_string())?;
        out.free_mode = Some(FreeMode::parse(raw)?);
    } else if let Some(v) = obj.get("free_only").and_then(Value::as_bool) {
        out.free_mode = Some(if v { FreeMode::Free } else { FreeMode::Any });
    }
    if let Some(Some(v)) = opt_i64(obj, "min_seeders")? {
        out.min_seeders = Some(i32::try_from(v).unwrap_or(i32::MAX));
    }
    if let Some(opt) = opt_i64(obj, "max_seeders")? {
        out.max_seeders = Some(opt.map(|n| i32::try_from(n).unwrap_or(i32::MAX)));
    }
    if let Some(opt) = opt_i64(obj, "min_leechers")? {
        out.min_leechers = Some(opt.map(|n| i32::try_from(n).unwrap_or(i32::MAX)));
    }
    if let Some(Some(v)) = opt_i64(obj, "hold_days")? {
        out.hold_days = Some(v);
    }
    if let Some(Some(v)) = opt_u64(obj, "max_adds_per_round")? {
        if v == 0 {
            return Err("max_adds_per_round must be >= 1".into());
        }
        out.max_adds_per_round = Some(usize::try_from(v.clamp(1, 100)).unwrap_or(20));
    }
    if let Some(raw) = opt_trimmed_str(obj, "include_regex")? {
        out.include_regex = Some(raw.as_deref().map(CompiledRegex::compile).transpose()?);
    }
    if let Some(raw) = opt_trimmed_str(obj, "exclude_regex")? {
        out.exclude_regex = Some(raw.as_deref().map(CompiledRegex::compile).transpose()?);
    }
    if let Some(raw) = opt_trimmed_str(obj, "active_hours")? {
        out.active_hours = Some(raw.as_deref().map(ActiveHours::parse).transpose()?);
    }
    if let Some(raw) = opt_trimmed_str(obj, "relative_directory")? {
        if let Some(dir) = &raw {
            validate_relative_directory(dir)?;
        }
        out.relative_directory = Some(raw);
    }
    if let Some(raw) = opt_trimmed_str(obj, "category")? {
        if let Some(cat) = &raw {
            downloader::validate_category_name(cat).map_err(str::to_string)?;
        }
        out.category = Some(raw);
    }
    if let Some(val) = obj.get("tags") {
        let arr = val
            .as_array()
            .ok_or_else(|| "tags must be an array".to_string())?;
        let mut tags = Vec::new();
        for item in arr {
            let tag = item
                .as_str()
                .map(str::trim)
                .ok_or_else(|| "tag must be a string".to_string())?;
            validate_boost_tag(tag)?;
            if !tags.iter().any(|existing: &String| existing == tag) {
                tags.push(tag.to_string());
            }
        }
        out.tags = Some(tags);
    }
    let up_key = if obj.contains_key("upload_limit_bytes") {
        "upload_limit_bytes"
    } else {
        "upload_limit"
    };
    out.upload_limit_bytes = opt_u64(obj, up_key)?;
    let dn_key = if obj.contains_key("download_limit_bytes") {
        "download_limit_bytes"
    } else {
        "download_limit"
    };
    out.download_limit_bytes = opt_u64(obj, dn_key)?;
    Ok(())
}

fn parse_rules_array(
    val: &Value,
    defaults: &EffectiveBoostRule,
) -> Result<Vec<BoostRuleConfig>, String> {
    let arr = val
        .as_array()
        .ok_or_else(|| "rules must be an array".to_string())?;
    let mut seen = HashSet::new();
    let mut rules = Vec::with_capacity(arr.len());
    for item in arr {
        let obj = item
            .as_object()
            .ok_or_else(|| "each entry in rules must be an object".to_string())?;
        let sid_raw = obj
            .get("site_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "rule site_id is required".to_string())?;
        let site_id: SiteId = sid_raw
            .parse()
            .map_err(|_| format!("invalid rule site_id `{sid_raw}`"))?;
        if !seen.insert(site_id) {
            return Err(format!("duplicate boost rule for site_id `{site_id}`"));
        }
        let enabled = obj.get("enabled").and_then(Value::as_bool).unwrap_or(true);
        let overrides = BoostRuleOverrides::from_object(obj)?;
        let mut merged = defaults.clone();
        merged.apply_overrides(&overrides, true);
        validate_effective_ranges(&merged)?;
        rules.push(BoostRuleConfig {
            site_id,
            enabled,
            overrides,
        });
    }
    Ok(rules)
}

fn validate_effective_ranges(rule: &EffectiveBoostRule) -> Result<(), String> {
    if rule.max_seeders.is_some_and(|m| m < rule.min_seeders) {
        return Err("max_seeders must be >= min_seeders".into());
    }
    if let (Some(min_sz), Some(max_sz)) = (rule.min_size_bytes, rule.max_size_bytes)
        && max_sz < min_sz
    {
        return Err("max_size_bytes must be >= min_size_bytes".into());
    }
    Ok(())
}

fn validate_relative_directory(raw: &str) -> Result<(), String> {
    if raw.contains('\0')
        || raw.chars().any(char::is_control)
        || raw.contains('\\')
        || raw.starts_with('/')
        || raw.ends_with('/')
    {
        return Err("invalid relative_directory".into());
    }
    let mut has_seg = false;
    for comp in Path::new(raw).components() {
        match comp {
            Component::Normal(seg) if seg.to_str().is_some() => has_seg = true,
            _ => return Err("unsafe path component in relative_directory".into()),
        }
    }
    has_seg
        .then_some(())
        .ok_or_else(|| "empty relative_directory".into())
}

fn validate_boost_tag(tag: &str) -> Result<(), String> {
    if tag.is_empty() || tag.contains(',') || tag.chars().any(char::is_control) {
        return Err(format!("invalid download tag `{tag}`"));
    }
    if downloader::is_reserved_system_tag(tag) {
        return Err(format!("reserved download tag `{tag}` is not permitted"));
    }
    Ok(())
}

fn opt_i64(obj: &Map<String, Value>, key: &str) -> Result<Option<Option<i64>>, String> {
    let Some(val) = obj.get(key) else {
        return Ok(None);
    };
    if val.is_null() {
        return Ok(Some(None));
    }
    let n = val
        .as_i64()
        .ok_or_else(|| format!("{key} must be an integer"))?;
    (n >= 0)
        .then_some(Some(Some(n)))
        .ok_or_else(|| format!("{key} must be non-negative"))
}

fn opt_u64(obj: &Map<String, Value>, key: &str) -> Result<Option<Option<u64>>, String> {
    let Some(val) = obj.get(key) else {
        return Ok(None);
    };
    if val.is_null() {
        return Ok(Some(None));
    }
    val.as_u64()
        .map(|n| Some(Some(n)))
        .ok_or_else(|| format!("{key} must be a non-negative integer"))
}

fn opt_f64(obj: &Map<String, Value>, key: &str) -> Result<Option<Option<f64>>, String> {
    let Some(val) = obj.get(key) else {
        return Ok(None);
    };
    if val.is_null() {
        return Ok(Some(None));
    }
    let f = val
        .as_f64()
        .ok_or_else(|| format!("{key} must be a number"))?;
    (f.is_finite() && f >= 0.0)
        .then_some(Some(Some(f)))
        .ok_or_else(|| format!("{key} must be a finite non-negative number"))
}

fn opt_trimmed_str(obj: &Map<String, Value>, key: &str) -> Result<Option<Option<String>>, String> {
    let Some(val) = obj.get(key) else {
        return Ok(None);
    };
    match val {
        Value::Null => Ok(Some(None)),
        Value::String(s) => {
            let t = s.trim();
            Ok(Some((!t.is_empty()).then(|| t.to_string())))
        }
        _ => Err(format!("{key} must be a string")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn active_hours_and_rule_overrides_work() {
        let normal = ActiveHours::parse("01:00-03:00").unwrap();
        assert!(
            normal.is_active_at(3600, Some("UTC")) && !normal.is_active_at(3 * 3600, Some("UTC"))
        );
        let wrap = ActiveHours::parse("22:00-06:00").unwrap();
        assert!(
            wrap.is_active_at(23 * 3600, Some("UTC"))
                && wrap.is_active_at(15 * 3600, Some("Asia/Shanghai"))
        );
        assert!(!wrap.is_active_at(8 * 3600, Some("Asia/Shanghai")));
        assert!(ActiveHours::parse("25:00-01:00").is_err());

        let (s1, s2) = (SiteId::new(), SiteId::new());
        let cfg = BoostConfig::from_layers(&[&json!({
            "sites": [s1.to_string(), s2.to_string()],
            "free_mode": "free", "hold_days": 3, "relative_directory": "brush/base",
            "tags": ["base-tag"], "include_regex": "1080p",
            "rules": [{ "site_id": s2.to_string(), "free_mode": "2x_free", "hold_days": 9, "tags": ["site2-tag"], "sequential": true }]
        })]).unwrap();
        assert_eq!(cfg.effective_for_site(Some(s1)).free_mode, FreeMode::Free);
        let r2 = cfg.effective_for_site(Some(s2));
        assert_eq!(r2.free_mode, FreeMode::TwoXFree);
        assert_eq!(r2.tags, vec!["base-tag", "site2-tag"]);
        assert_eq!(cfg.effective_hold_days(Some(s2)), 9);
        assert!(
            BoostConfig::from_layers(&[&json!({ "relative_directory": "../escape" })]).is_err()
        );
        assert!(BoostConfig::from_layers(&[&json!({ "include_regex": "[" })]).is_err());
    }
}
