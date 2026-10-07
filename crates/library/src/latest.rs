//! 「最近入库」这一行的观看状态口径，两个面各自取用。
//!
//! ## 自有 API（我们的 UI）：分级排序，不隐藏任何条目
//!
//! 首页库行与「我的收藏」行的「未看优先」档 = [`WatchTier`]：没开始看过的排最前、
//! 在看其次、已看完沉底，**段内仍按这一行自己的排序**（「最近添加」就是入库时间倒序）。
//!
//! 这里刻意不筛：一个小库（比如只有 3 部剧、其中 2 部看过）筛完只剩 1 张卡，看起来
//! 像坏了；而"最近添加"这个名字本来就是描述入库时间的，把看过的藏起来反而名不副实。
//! 客户端传 `unwatched_first=true` 时才这么排；墙上用户手选的「未观看」是严格筛，
//! 与这里无关。
//!
//! ## Jellyfin 客户端表面：筛，但整段都看过时不留空
//!
//! `Items/Latest` 的 `IsPlayed` 是**协议给的筛选**：客户端显式传了 `true`/`false` 就
//! 严格照办。没传时按服务端默认策略走——Jellyfin 自己的默认策略（用户配置
//! `HidePlayedInLatest`，出厂 `true`）是只回没看过的，整段都看过了就返回空列表，
//! 于是客户端首页上那个库连入口都没有。我们用 [`prefer_unwatched`] 多走一步：有没看过
//! 的就只回没看过的，一部都没有时才回全部。
//!
//! 两个面口径不同是**有意的**：那边是客户端契约（`IsPlayed=false` 必须只回没看过的），
//! 这边是我们自己的呈现偏好。

/// 一部作品的观看分级。声明顺序即排序顺序：未看 → 在看 → 已看完。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum WatchTier {
    /// 没开始看过：没有进度、没有播放次数、没有看完标记
    #[default]
    Unwatched,
    /// 在看：动过但没看完
    Watching,
    /// 已看完：作品级的看完标记
    Finished,
}

impl WatchTier {
    /// `seen` = 动过（有进度或播放次数）；`played` = 作品级的"已看完"。
    /// 两个口径与筛选条的「未观看 / 在看 / 已看完」逐字对应，只是这里不筛只排。
    pub fn of(seen: bool, played: bool) -> Self {
        match (seen, played) {
            (false, _) => Self::Unwatched,
            (true, true) => Self::Finished,
            (true, false) => Self::Watching,
        }
    }
}

/// 未观看优先：`is_unwatched` 为真的条目非空就只保留它们，否则原样返回全部
/// （连顺序都不动——排序是调用方的事，这里只答"给哪一批"）。
pub fn prefer_unwatched<T>(items: Vec<T>, is_unwatched: impl Fn(&T) -> bool) -> Vec<T> {
    if items.iter().any(|item| is_unwatched(item)) {
        items.into_iter().filter(|item| is_unwatched(item)).collect()
    } else {
        items
    }
}

#[cfg(test)]
mod tests {
    use super::{WatchTier, prefer_unwatched};

    #[test]
    fn keeps_unwatched_when_any_exists() {
        let items = vec![("a", true), ("b", false), ("c", true)];
        let kept = prefer_unwatched(items, |(_, unwatched)| *unwatched);
        assert_eq!(kept, vec![("a", true), ("c", true)]);
    }

    #[test]
    fn falls_back_to_everything_when_all_watched() {
        let items = vec![("a", false), ("b", false)];
        let kept = prefer_unwatched(items, |(_, unwatched)| *unwatched);
        assert_eq!(kept, vec![("a", false), ("b", false)]);
    }

    #[test]
    fn empty_stays_empty() {
        let items: Vec<(&str, bool)> = Vec::new();
        let kept = prefer_unwatched(items, |(_, unwatched)| *unwatched);
        assert!(kept.is_empty());
    }

    #[test]
    fn tiers_order_unwatched_then_watching_then_finished() {
        assert_eq!(WatchTier::of(false, false), WatchTier::Unwatched);
        assert_eq!(WatchTier::of(true, false), WatchTier::Watching);
        assert_eq!(WatchTier::of(true, true), WatchTier::Finished);
        // 「动过但没看完」按在看算，不会因为整剧级标记缺失被当成没开始
        assert_eq!(WatchTier::of(false, true), WatchTier::Unwatched);
        assert!(WatchTier::Unwatched < WatchTier::Watching);
        assert!(WatchTier::Watching < WatchTier::Finished);
    }
}
