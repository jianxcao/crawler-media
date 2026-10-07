use crate::management::ApiState;

/// Search using original and display names, their year variants, and a useful
/// short title. Results are de-duplicated across keywords.
pub(crate) fn search_torrents_for_media(
    state: &ApiState,
    sites: &[domain::Site],
    media: &domain::Media,
) -> (Vec<domain::Torrent>, Vec<String>, bool, Vec<String>) {
    let mut keywords = Vec::new();
    if let Some(original) = media
        .original_title
        .as_deref()
        .filter(|title| !title.is_empty())
    {
        keywords.push(original.to_string());
    }
    if !media.title.is_empty() {
        keywords.push(media.title.clone());
        if let Some(short) = short_title(&media.title) {
            keywords.push(short);
        }
    }
    if let Some(year) = media.year {
        let title = media.title.trim();
        if !title.is_empty() {
            keywords.push(format!("{title} {year}"));
        }
        if let Some(original) = media
            .original_title
            .as_deref()
            .filter(|title| !title.is_empty())
        {
            keywords.push(format!("{} {year}", original.trim()));
        }
    }
    let mut seen_keywords = std::collections::HashSet::new();
    keywords.retain(|keyword| seen_keywords.insert(keyword.clone()));
    search_keywords(state, sites, keywords)
}

fn search_keywords(
    state: &ApiState,
    sites: &[domain::Site],
    keywords: Vec<String>,
) -> (Vec<domain::Torrent>, Vec<String>, bool, Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    let mut merged = Vec::new();
    let mut succeeded = keywords.is_empty() || sites.is_empty();
    let mut failures = Vec::new();
    for keyword in &keywords {
        let outcome = state.indexer.search(sites, keyword);
        succeeded |=
            report_site_failures(&outcome, sites, &format!("search {keyword}"), &mut failures);
        for torrent in outcome.torrents {
            if seen.insert(torrent.enclosure.clone()) {
                merged.push(torrent);
            }
        }
    }
    (merged, keywords, succeeded, failures)
}

pub(crate) fn report_site_failures(
    outcome: &indexer::SearchOutcome,
    sites: &[domain::Site],
    operation: &str,
    failures: &mut Vec<String>,
) -> bool {
    for failure in &outcome.failures {
        let site_name = sites
            .iter()
            .find(|s| s.id == failure.site_id)
            .map(|s| s.name.as_str())
            .unwrap_or("未知站点");
        let detail = format!("{}: {}", site_name, failure.error);
        tracing::warn!(site_id = %failure.site_id, site = %site_name, operation, error = %failure.error, "站点搜索失败");
        failures.push(detail);
    }
    sites.is_empty() || outcome.failures.len() < sites.len()
}

fn short_title(title: &str) -> Option<String> {
    let cut = title
        .split(['：', ':', '（', '(', '「', '『'])
        .next()
        .unwrap_or(title)
        .trim();
    (cut != title && !cut.is_empty()).then(|| cut.to_string())
}
