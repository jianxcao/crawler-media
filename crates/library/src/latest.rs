//! 「最近入库」这一行的观看状态口径：**只排不筛**，两个面各自取用。
//!
//! 两个面都是"没看完的排前面、看完了的沉底"，段内仍是调用方自己的排序
//! （「最近添加」就是入库时间倒序）。区别只在分级粒度：
//!
//! - **自有 API（我们的 UI）**：首页库行与「我的收藏」行的「未看优先」档 = [`WatchTier`]
//!   （未看 → 在看 → 已看完）。客户端传 `unwatched_first=true` 时才这么排。
//! - **Jellyfin 客户端表面**：`Items/Latest` 缺省用 [`played_last`]——那边只有条目级的
//!   `played` 一档，没有逐集的"在看"。
//!
//! 为什么不筛：一个小库（比如只有 3 部剧、其中 2 部看过）筛完只剩 1 张卡，看起来像坏了；
//! 而"最近添加"这个名字本来就是描述入库时间的，把看过的藏起来反而名不副实。Jellyfin
//! 自己缺省会按用户配置 `HidePlayedInLatest`（出厂 `true`）把已看完的藏掉，全看过的库
//! 更是直接返回空列表——客户端首页上那个库连入口都没有；这是我们有意的偏差。
//!
//! 协议里唯一不动的那条：**显式** `IsPlayed=true|false` 永远是严格筛（客户端要什么给
//! 什么），`w=unwatched` 这类墙上手选的筛选也一样。

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

/// 已看完的沉底：`is_played` 为真的排到后面，其余保持原顺序。
///
/// Jellyfin 面的 `Items/Latest` 缺省用它（那里只有条目级的 `played` 一档，没有
/// 逐集的"在看"），所以是稳定排序：调用方先按入库时间倒序排好，段内就还是那个序。
pub fn played_last<T>(items: Vec<T>, is_played: impl Fn(&T) -> bool) -> Vec<T> {
    let mut items = items;
    items.sort_by_key(|item| is_played(item));
    items
}

#[cfg(test)]
mod tests {
    use super::{WatchTier, played_last};

    #[test]
    fn played_goes_last_and_keeps_the_inner_order() {
        // 段内顺序（这里是调用方排好的入库时间倒序）不能被这一步打乱
        let items = vec![("new-unplayed", false), ("new-played", true), ("old-unplayed", false)];
        let ordered = played_last(items, |(_, played)| *played);
        assert_eq!(
            ordered,
            vec![("new-unplayed", false), ("old-unplayed", false), ("new-played", true)]
        );
    }

    #[test]
    fn all_played_keeps_the_original_order() {
        let items = vec![("a", true), ("b", true)];
        assert_eq!(played_last(items, |(_, played)| *played), vec![("a", true), ("b", true)]);
    }

    #[test]
    fn empty_stays_empty() {
        let items: Vec<(&str, bool)> = Vec::new();
        assert!(played_last(items, |(_, played)| *played).is_empty());
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
