//! 排序：多项 `ProtocolSort` 直接翻成 `ORDER BY`，末尾固定收敛项。
//!
//! 排序键映射与旧比较器一一对应：`Name` → 小写标题，`DateCreated` → 入库排序值，
//! `SeasonNumber`/`EpisodeNumber` → 分集行的季/集号（媒体级行没有这两列，跳过）。
//! 未指定排序时沿用旧缺省：全分集按季/集升序，否则入库时间与标题倒序。

use domain::{ProtocolItemQuery, ProtocolSortKey};

/// 生成 `ORDER BY` 的列清单；`alias` 是外层 SELECT 的别名。
pub(super) fn order_by_sql(
    request: &ProtocolItemQuery,
    episode_level: bool,
    alias: &str,
) -> String {
    let mut keys = Vec::new();
    for sort in &request.sort {
        let columns = columns(sort.key, episode_level, alias);
        if columns.is_empty() {
            continue;
        }
        let direction = if sort.descending { "DESC" } else { "ASC" };
        keys.extend(
            columns
                .into_iter()
                .map(|column| format!("{column} {direction}")),
        );
    }
    if keys.is_empty() {
        keys = default_keys(episode_level, alias);
    }
    // 只有大小写不同的标题：所有请求的键都比完之后才用原串定序（R23），
    // 不让原串抢在后续请求键（如 DateCreated）之前。
    if request.sort.iter().any(|sort| sort.key == ProtocolSortKey::Name) {
        keys.push(format!("{alias}.title ASC"));
    }
    // 收敛项与旧实现的最终 tie-break（media id）一致，并让分页在并列时不抖动。
    for tie in ["library_id", "media_id", "season", "episode"] {
        keys.push(format!("{alias}.{tie} ASC"));
    }
    keys.join(", ")
}

/// `Name` 只贡献 Unicode 折叠后的主键（`casefold`，与搜索同一折叠）；原串 tie-break
/// 由 `order_by_sql` 放在全部请求键之后。
fn columns(key: ProtocolSortKey, episode_level: bool, alias: &str) -> Vec<String> {
    match key {
        ProtocolSortKey::Name => vec![format!("casefold({alias}.title)")],
        ProtocolSortKey::DateCreated => vec![format!("{alias}.added_at_sort")],
        ProtocolSortKey::SeasonNumber if episode_level => vec![format!("{alias}.season")],
        ProtocolSortKey::EpisodeNumber if episode_level => vec![format!("{alias}.episode")],
        ProtocolSortKey::SeasonNumber | ProtocolSortKey::EpisodeNumber | ProtocolSortKey::Noop => {
            Vec::new()
        }
    }
}

fn default_keys(episode_level: bool, alias: &str) -> Vec<String> {
    if episode_level {
        vec![
            format!("{alias}.season ASC"),
            format!("{alias}.episode ASC"),
        ]
    } else {
        vec![
            format!("{alias}.added_at_sort DESC"),
            format!("casefold({alias}.title) DESC"),
        ]
    }
}
