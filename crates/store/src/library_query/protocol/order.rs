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
    // 收敛项与旧实现的最终 tie-break（media id）一致，并让分页在并列时不抖动。
    for tie in ["library_id", "media_id", "season", "episode"] {
        keys.push(format!("{alias}.{tie} ASC"));
    }
    keys.join(", ")
}

/// `Name` 展开成两列：折叠大小写的主键 + 原串次序键，让只有大小写不同的标题稳定可比
/// （SQLite 的 `lower()` 只折 ASCII，故用 BINARY 原串做 tie-break，不做二次折叠）。
fn columns(key: ProtocolSortKey, episode_level: bool, alias: &str) -> Vec<String> {
    match key {
        ProtocolSortKey::Name => vec![format!("lower({alias}.title)"), format!("{alias}.title")],
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
            format!("lower({alias}.title) DESC"),
        ]
    }
}
