//! 紧凑时间格式：`2026-09-23 07:12:48`（UTC，秒级）。
//! 默认 `SystemTime` 计时器输出 RFC3339 微秒级 `...48.755985Z`，
//! 日志不需要那么精确，这里只保留到秒，便于人读。

use std::time::{SystemTime, UNIX_EPOCH};

use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;

#[derive(Clone, Copy, Debug, Default)]
pub struct CompactTime;

impl FormatTime for CompactTime {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let (year, month, day) = civil_date(secs.div_euclid(86_400));
        let rem = secs.rem_euclid(86_400);
        let (hour, min, sec) = (rem / 3600, rem % 3600 / 60, rem % 60);

        write!(
            w,
            "{year:04}-{month:02}-{day:02} {hour:02}:{min:02}:{sec:02}"
        )
    }
}

pub(crate) fn format_rfc3339_utc(value: SystemTime) -> Option<String> {
    let elapsed = value.duration_since(UNIX_EPOCH).ok()?;
    let seconds = i64::try_from(elapsed.as_secs()).ok()?;
    let (year, month, day) = civil_date(seconds.div_euclid(86_400));
    let rem = seconds.rem_euclid(86_400);
    let (hour, minute, second) = (rem / 3600, rem % 3600 / 60, rem % 60);
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{:03}Z",
        elapsed.subsec_millis()
    ))
}

fn civil_date(days: i64) -> (i64, i64, i64) {
    // 公历日历推算（Howard Hinnant civil_from_days 算法），UTC。
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::format_rfc3339_utc;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn formats_rfc3339_utc_with_milliseconds() {
        assert_eq!(
            format_rfc3339_utc(UNIX_EPOCH + Duration::from_millis(1_700_000_000_123)),
            Some("2023-11-14T22:13:20.123Z".into())
        );
    }
}
