//! 「最近入库」的取数口径：未观看优先，整段都看过了才回退到全部。
//!
//! Jellyfin 的 `Items/Latest` 在用户开着 `HidePlayedInLatest`（出厂默认开着）时只回
//! 没看过的；全看过就返回空列表——一个看完的库在客户端首页于是完全没有入口，
//! 看起来像"我的剧不见了"。我们这条口径多走一步：有没看过的就只给没看过的，
//! 一部没看过的都没有时才给全部（顺序不动）。
//!
//! 两个面共用这一个函数，各自喂自己的"没看过"判定：
//!   - Jellyfin `/Users/{id}/Items/Latest`：客户端**没显式**给 `IsPlayed` 时，
//!     判定是条目级的 `!played`；
//!   - web 首页的库行（`/api/v1/libraries/{id}/items?w=unwatched&w_fallback=true`），
//!     判定是"没开始看过"（没播过、没进度、没播过计数）。
//!
//! 显式的 `IsPlayed` / `w` 永远是**严格**筛选，不经过这里：用户手选的"未观看"
//! 就该只有未观看，不能让回退把看过的塞回来。

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
    #[test]
    fn keeps_unwatched_when_any_exists() {
        let items = vec![("a", true), ("b", false), ("c", true)];
        let kept = super::prefer_unwatched(items, |(_, unwatched)| *unwatched);
        assert_eq!(kept, vec![("a", true), ("c", true)]);
    }

    #[test]
    fn falls_back_to_everything_when_all_watched() {
        let items = vec![("a", false), ("b", false)];
        let kept = super::prefer_unwatched(items, |(_, unwatched)| *unwatched);
        assert_eq!(kept, vec![("a", false), ("b", false)]);
    }

    #[test]
    fn empty_stays_empty() {
        let items: Vec<(&str, bool)> = Vec::new();
        let kept = super::prefer_unwatched(items, |(_, unwatched)| *unwatched);
        assert!(kept.is_empty());
    }
}
