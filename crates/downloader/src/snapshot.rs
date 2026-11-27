//! Client task snapshots and ownership markers.
/// One client-side task as reported by [`crate::Downloader::task_snapshots`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TaskSnapshot {
    /// Our marker tag in the client: exactly [`crate::TASK_TAG`], empty for anything
    /// else. Non-empty only for crawler-media deliveries — the task center uses
    /// it to filter foreign torrents out. Per-torrent identity comes from
    /// [`TaskSnapshot::owned_tags`], never from this marker.
    pub tag: String,
    /// Client-side task name (the torrent title).
    pub name: String,
    /// 0.0..=1.0.
    pub progress: f64,
    /// Raw client state string (mapped by the caller into its own vocabulary).
    pub state: String,
    pub size_bytes: u64,
    pub downloaded_bytes: u64,
    pub uploaded_bytes: u64,
    pub download_speed: u64,
    pub upload_speed: u64,
    pub info_hash: String,
    /// Every per-torrent ownership tag (`crawler-media-owned-<sha256>`) the
    /// client reports for this task. `tag` only carries the generic marker, so
    /// HTTP-enclosure identity must be proven through this list.
    pub owned_tags: Vec<String>,
    /// Share ratio reported by the client (0.0 when unavailable).
    pub ratio: f64,
    /// Total seeding duration in seconds reported by the client.
    pub seeding_secs: u64,
    /// UTC unix timestamp when the task was added to the client.
    pub added_at: Option<i64>,
    /// UTC unix timestamp when download completed.
    pub completed_at: Option<i64>,
    /// UTC unix timestamp of last peer/transfer activity.
    pub last_activity_at: Option<i64>,
    /// Downloader category assigned to this task.
    pub category: Option<String>,
    /// Non-system user/plugin tags attached to this task.
    pub user_tags: Vec<String>,
}

/// Collect `crawler-media-owned-*` and `cm-op:*` ownership tags from client tag/label values.
pub fn owned_tags_from<'a>(tags: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    tags.into_iter()
        .map(str::trim)
        .filter(|tag| tag.starts_with(OWNED_TAG_PREFIX) || tag.starts_with("cm-op:"))
        .map(str::to_string)
        .collect()
}

/// Collect non-system tags/labels from client tag values.
pub fn user_tags_from<'a>(tags: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    tags.into_iter()
        .map(str::trim)
        .filter(|tag| !tag.is_empty() && !is_reserved_system_tag(tag))
        .map(str::to_string)
        .collect()
}

/// Validate a downloader category name against control characters and path traversal.
/// Empty `""` is allowed (clears category assignment in qBittorrent).
pub fn validate_category_name(category: &str) -> Result<(), &'static str> {
    if category.is_empty() {
        return Ok(());
    }
    if category.trim() != category || category.chars().any(char::is_control) {
        return Err("download category contains leading/trailing whitespace or control characters");
    }
    if category.contains('\\')
        || category.starts_with('/')
        || category.ends_with('/')
        || category.contains("//")
    {
        return Err(
            "download category must be a clean relative name without backslashes or leading/trailing slashes",
        );
    }
    for seg in category.split('/') {
        if seg.is_empty()
            || seg.trim() != seg
            || seg == "."
            || seg == ".."
            || seg.as_bytes().get(1) == Some(&b':')
        {
            return Err("download category contains path traversal or drive prefix");
        }
    }
    Ok(())
}

/// Whether a single client tag/label proves the task belongs to this crawler-media instance.
pub(crate) fn is_instance_owned_tag(tag: &str, ownership_tag: &str) -> bool {
    let t = tag.trim();
    if t.is_empty() {
        return false;
    }
    let expected = ownership_tag.trim();
    (!expected.is_empty() && t == expected)
        || t == TASK_TAG
        || t.starts_with("cm-op:")
        || t.starts_with(OWNED_TAG_PREFIX)
}

/// Prefix of [`crate::ownership_tag`] values.
pub const OWNED_TAG_PREFIX: &str = "crawler-media-owned-";

/// Whether `tag` uses any reserved host/system tag or namespace prefix.
pub fn is_reserved_system_tag(tag: &str) -> bool {
    let t = tag.trim();
    t == TASK_TAG
        || t == CLAIMED_TAG
        || t.starts_with("crawler-media-")
        || t.starts_with("cm-")
        || t.starts_with("cm:")
        || t.starts_with("cm_")
}

/// The one tag every crawler-media delivery carries inside a download client.
///
/// A single recognizable tag (same value as the configured category) keeps the
/// client's tag list at exactly one entry no matter how many torrents we
/// deliver — no per-torrent `crawler-media-<hash>` pollution. Identity between
/// our Torrent and a client-side task is established by exact normalized title
/// + size ([`crate::normalize_name`] / [`crate::names_match`] / [`crate::size_matches`]).
pub const TASK_TAG: &str = "crawler-media";

/// Review H-3: extra tag/label on a user's existing task that we claimed
/// (same info hash). Its data belongs to the user: never delete its files
/// and never Move them out of the download directory.
pub const CLAIMED_TAG: &str = "crawler-media-claimed";

#[cfg(test)]
mod owned_tag_tests {
    #[test]
    fn owned_tags_are_collected_and_generic_tags_ignored() {
        let owned = crate::ownership_tag("https://pt.example/a");
        let raw = format!("other, crawler-media ,{owned}, crawler-media-unknown");
        assert_eq!(super::owned_tags_from(raw.split(',')), vec![owned]);
        assert!(super::owned_tags_from(["crawler-media", ""]).is_empty());
    }
}
