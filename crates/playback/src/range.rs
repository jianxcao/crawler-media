#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    pub fn len(self) -> u64 {
        self.end.saturating_sub(self.start) + 1
    }
}

pub fn parse_range(header: &str, size: u64) -> Option<ByteRange> {
    if size == 0 {
        return None;
    }
    let spec = header.strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (start, end) = spec.split_once('-')?;
    if start.is_empty() {
        let suffix: u64 = end.parse().ok()?;
        if suffix == 0 {
            return None;
        }
        return Some(ByteRange {
            start: size.saturating_sub(suffix),
            end: size - 1,
        });
    }
    let start: u64 = start.parse().ok()?;
    let end = if end.is_empty() {
        size - 1
    } else {
        end.parse::<u64>().ok()?.min(size - 1)
    };
    if start > end || start >= size {
        tracing::warn!(header, size, start, end, "无效的字节范围请求");
        return None;
    }
    let range = ByteRange { start, end };
    tracing::debug!(
        header,
        size,
        start = range.start,
        end = range.end,
        "解析字节范围请求"
    );
    Some(range)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffix_and_open_ended_ranges_follow_http_byte_semantics() {
        assert_eq!(
            parse_range("bytes=-4", 16),
            Some(ByteRange { start: 12, end: 15 })
        );
        assert_eq!(
            parse_range("bytes=-50", 16),
            Some(ByteRange { start: 0, end: 15 })
        );
        assert_eq!(
            parse_range("bytes=14-99", 16),
            Some(ByteRange { start: 14, end: 15 })
        );
        assert_eq!(
            parse_range("bytes=4-", 16),
            Some(ByteRange { start: 4, end: 15 })
        );
        assert_eq!(parse_range("bytes=16-", 16), None);
        assert_eq!(parse_range("bytes=0-0", 0), None);
        assert_eq!(parse_range("bytes=-0", 16), None);
        assert_eq!(parse_range("bytes=1-2,4-5", 16), None);
    }
}
