use std::collections::{HashMap, HashSet};
use std::path::{Component, Path};

use plugin_api::{DownloadPlanPatch, DownloadPlanView, Failure, SearchQuery, SearchQueryPatch};

pub fn rescore_delta_cap(scores: &[i32]) -> i32 {
    let Some((&min_s, &max_s)) = scores.iter().min().zip(scores.iter().max()) else {
        return 10;
    };
    let span = i64::from(max_s) - i64::from(min_s);
    let ten_pct = span / 10;
    ten_pct.max(10).min(i64::from(i32::MAX)) as i32
}

#[derive(Clone)]
pub struct RescoreBudget {
    cap: i32,
    used: HashMap<(String, String), i32>,
}

impl RescoreBudget {
    pub fn new(scores: &[i32]) -> Self {
        Self {
            cap: rescore_delta_cap(scores),
            used: HashMap::new(),
        }
    }

    pub fn try_apply(&mut self, plugin_id: &str, handle: &str, delta: i32) -> Result<i32, Failure> {
        let key = (plugin_id.to_string(), handle.to_string());
        let prev = self.used.get(&key).copied().unwrap_or(0);
        let next = prev
            .checked_add(delta)
            .ok_or_else(|| Failure::InvalidPatch("rescore delta overflow".into()))?;
        if next.unsigned_abs() > self.cap.unsigned_abs() {
            return Err(Failure::InvalidPatch(format!(
                "rescore delta {next} exceeds cumulative cap {}",
                self.cap
            )));
        }
        self.used.insert(key, next);
        Ok(next)
    }
}

fn validate_normal_rel_path(raw: &str) -> Result<String, Failure> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.contains('\0')
        || trimmed.chars().any(char::is_control)
        || trimmed.contains('\\')
        || trimmed.starts_with('/')
        || trimmed.ends_with('/')
    {
        return Err(Failure::InvalidPatch("invalid relative path".into()));
    }
    let path = Path::new(trimmed);
    let mut parts = Vec::new();
    for comp in path.components() {
        match comp {
            Component::Normal(seg) => {
                let s = seg
                    .to_str()
                    .ok_or_else(|| Failure::InvalidPatch("non-utf8 path segment".into()))?;
                parts.push(s);
            }
            _ => return Err(Failure::InvalidPatch("unsafe path component".into())),
        }
    }
    if parts.is_empty() {
        return Err(Failure::InvalidPatch("empty relative path".into()));
    }
    Ok(parts.join("/"))
}

pub fn validate_and_apply_download_plan_patch(
    plan: &mut DownloadPlanView,
    patch: &DownloadPlanPatch,
    allowed_downloaders: &[String],
    plugin_id: &str,
) -> Result<(), Failure> {
    if let Some(dl) = &patch.downloader_id
        && !allowed_downloaders.iter().any(|allowed| allowed == dl)
    {
        return Err(Failure::InvalidPatch(format!(
            "downloader `{dl}` is not permitted"
        )));
    }
    if let Some(dir) = &patch.relative_directory {
        validate_normal_rel_path(dir)?;
    }
    if let Some(cat) = &patch.category {
        validate_safe_category(cat)?;
    }
    let ns_prefix = format!("plugin:{plugin_id}:");
    for tag in patch.add_tags.iter().chain(patch.remove_tags.iter()) {
        if tag.trim() != tag || tag.contains(',') || tag.chars().any(char::is_control) {
            return Err(Failure::InvalidPatch(format!(
                "plugin tag `{tag}` contains invalid characters or whitespace"
            )));
        }
        if !tag.starts_with(&ns_prefix) || tag.len() <= ns_prefix.len() {
            return Err(Failure::InvalidPatch(format!(
                "plugin tag `{tag}` must start with `{ns_prefix}`"
            )));
        }
    }

    if let Some(dl) = &patch.downloader_id {
        plan.downloader_id = dl.clone();
    }
    if let Some(dir) = &patch.relative_directory {
        plan.relative_directory = Some(validate_normal_rel_path(dir)?);
    }
    if let Some(cat) = &patch.category {
        plan.category = Some(cat.clone());
    }
    for rem in &patch.remove_tags {
        plan.tags.retain(|t| t != rem);
    }
    for add in &patch.add_tags {
        if !plan.tags.contains(add) {
            plan.tags.push(add.clone());
        }
    }
    if let Some(paused) = patch.paused {
        plan.paused = paused;
    }
    if patch.upload_limit.is_some() {
        plan.upload_limit = patch.upload_limit;
    }
    if patch.download_limit.is_some() {
        plan.download_limit = patch.download_limit;
    }
    if let Some(seq) = patch.sequential {
        plan.sequential = seq;
    }
    if let Some(flp) = patch.first_last_piece {
        plan.first_last_piece = flp;
    }
    Ok(())
}

pub fn validate_and_apply_search_query_patch(
    query: &mut SearchQuery,
    site_ids: &mut Vec<String>,
    patch: &SearchQueryPatch,
) -> Result<(), Failure> {
    if let Some(next_sites) = &patch.site_ids {
        if next_sites.is_empty() {
            return Err(Failure::InvalidPatch(
                "search_query patch site_ids cannot be empty".into(),
            ));
        }
        let allowed: HashSet<&str> = site_ids.iter().map(String::as_str).collect();
        let mut seen = HashSet::new();
        for site in next_sites {
            if !allowed.contains(site.as_str()) || !seen.insert(site.as_str()) {
                return Err(Failure::InvalidPatch(format!(
                    "site `{site}` is outside allowed subset or duplicated"
                )));
            }
        }
    }
    let next_keyword = match &patch.keyword {
        Some(kw) => {
            let trimmed = kw.trim();
            if trimmed.is_empty() {
                return Err(Failure::InvalidPatch("keyword cannot be empty".into()));
            }
            Some(trimmed.to_string())
        }
        None => None,
    };

    if let Some(next_sites) = &patch.site_ids {
        *site_ids = next_sites.clone();
    }
    if let Some(keyword) = next_keyword {
        query.keyword = keyword;
    }
    if let Some(cats) = &patch.categories {
        query.categories = cats.clone();
    }
    Ok(())
}

pub fn validate_select_handles(
    admitted_handles: &[String],
    selected: &[String],
) -> Result<Vec<String>, Failure> {
    let allowed: HashSet<&str> = admitted_handles.iter().map(String::as_str).collect();
    let mut seen = HashSet::new();
    for handle in selected {
        if !allowed.contains(handle.as_str()) || !seen.insert(handle.as_str()) {
            return Err(Failure::InvalidPatch(format!(
                "selected handle `{handle}` is not admitted or duplicated"
            )));
        }
    }
    Ok(selected.to_vec())
}

pub fn validate_rename_path(path: &str) -> Result<String, Failure> {
    validate_normal_rel_path(path)
}

fn validate_safe_category(cat: &str) -> Result<(), Failure> {
    if cat.is_empty() {
        return Ok(());
    }
    if cat.trim() != cat
        || cat.chars().any(char::is_control)
        || cat.contains('\\')
        || cat.starts_with('/')
        || cat.ends_with('/')
        || cat.contains("//")
    {
        return Err(Failure::InvalidPatch(
            "download plan category contains invalid characters or slashes".into(),
        ));
    }
    for seg in cat.split('/') {
        if seg.is_empty()
            || seg.trim() != seg
            || seg == "."
            || seg == ".."
            || seg.as_bytes().get(1) == Some(&b':')
        {
            return Err(Failure::InvalidPatch(
                "download plan category contains path traversal or drive prefix".into(),
            ));
        }
    }
    Ok(())
}
