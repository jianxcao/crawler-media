//! 显式分页窗口：`limit`/`offset` 的校验与溢出安全切片。
//!
//! 省略 `limit` 时按 [`MAX_PAGE_LIMIT`] 返回一页；显式参数从"静默接受任意值"
//! 改成"越界报错"，切片对 `offset + limit` 溢出免疫。

use std::collections::HashMap;

/// Default and maximum list window; offsets remain absolute for item-index jumps.
pub(crate) const MAX_PAGE_LIMIT: usize = 500;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PageWindow {
    pub offset: usize,
    pub limit: usize,
}

impl PageWindow {
    pub(crate) fn parse(query: &HashMap<String, String>) -> Result<Self, String> {
        let offset = match query.get("offset") {
            None => 0,
            Some(raw) => raw.parse::<usize>().map_err(|_| format!("offset 必须是非负整数: {raw}"))?,
        };
        let limit = match query.get("limit") {
            None => MAX_PAGE_LIMIT,
            Some(raw) => {
                let value = raw.parse::<usize>().map_err(|_| format!("limit 必须是正整数: {raw}"))?;
                if value == 0 || value > MAX_PAGE_LIMIT {
                    return Err(format!("limit 必须在 1..={MAX_PAGE_LIMIT} 之间: {raw}"));
                }
                value
            }
        };
        Ok(Self { offset, limit })
    }

    /// 溢出安全切片：`offset` 超过长度时得到空区间；`offset + limit` 不会回绕。
    pub(crate) fn bounds(&self, len: usize) -> std::ops::Range<usize> {
        let start = self.offset.min(len);
        let end = start.saturating_add(self.limit).min(len);
        start..end
    }
}

#[cfg(test)]
mod tests {
    use super::{PageWindow, MAX_PAGE_LIMIT};

    fn query(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn omitted_limit_defaults_to_a_bounded_page() {
         let window = PageWindow::parse(&query(&[])).unwrap();
        assert_eq!(window.limit, MAX_PAGE_LIMIT);
        assert_eq!(window.bounds(MAX_PAGE_LIMIT + 1), 0..MAX_PAGE_LIMIT);
    }

    #[test]
    fn explicit_limit_is_validated_not_clamped() {
        for bad in ["0", "501", "-1", "abc", ""] {
            assert!(
                PageWindow::parse(&query(&[("limit", bad)])).is_err(),
                "limit={bad} 必须被拒绝"
            );
        }
        assert_eq!(
            PageWindow::parse(&query(&[("limit", "200")])).unwrap().limit,
            200
        );
        assert_eq!(
            PageWindow::parse(&query(&[("limit", "1")])).unwrap().limit,
            1
        );
    }

    #[test]
    fn offset_is_validated_and_bounds_never_overflow() {
        for bad in ["-1", "abc", ""] {
            assert!(
                PageWindow::parse(&query(&[("offset", bad)])).is_err(),
                "offset={bad} 必须被拒绝"
            );
        }
        let window = PageWindow::parse(&query(&[
            ("offset", &usize::MAX.to_string()),
            ("limit", "10"),
        ]))
        .unwrap();
        assert_eq!(window.bounds(10), 10..10, "offset 超出长度应为空区间");
        let window = PageWindow::parse(&query(&[
            ("offset", &(usize::MAX - 5).to_string()),
            ("limit", "200"),
        ]))
        .unwrap();
        assert_eq!(window.bounds(10), 10..10);
    }

    #[test]
    fn bounds_windows_the_middle_of_a_list() {
        let window = PageWindow::parse(&query(&[("offset", "2"), ("limit", "3")])).unwrap();
        assert_eq!(window.bounds(10), 2..5);
        assert_eq!(window.bounds(3), 2..3);
        assert_eq!(window.bounds(1), 1..1);
    }
}
