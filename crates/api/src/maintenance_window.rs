use std::path::{Component, Path, PathBuf};

use time::{Date, Month, OffsetDateTime, UtcOffset};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaintenanceWindow {
    pub timezone: String,
    pub window_key: String,
    pub window_start_utc: i64,
    pub window_end_utc: i64,
}

#[derive(Clone, Debug)]
enum TimeZoneSpec {
    Fixed(i32),
    Tzif(TzifData),
}

#[derive(Clone, Debug)]
struct TzifData {
    transitions: Vec<i64>,
    type_indices: Vec<u8>,
    ut_offsets: Vec<i32>,
    footer: Option<PosixTzRule>,
}

#[derive(Clone, Debug)]
struct PosixTzRule {
    std_offset: i32,
    dst: Option<PosixDstRule>,
}

#[derive(Clone, Debug)]
struct PosixDstRule {
    dst_offset: i32,
    start: PosixChangeRule,
    end: PosixChangeRule,
}

#[derive(Clone, Copy, Debug)]
struct PosixChangeRule {
    month: u8,
    week: u8,
    weekday: u8,
    secs_of_day: i32,
}

pub fn validate_timezone(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    let _ = parse_timezone(trimmed)?;
    Ok(trimmed.to_string())
}

pub fn compute_window(tz: &str, now_utc: i64) -> Result<MaintenanceWindow, String> {
    let canonical = tz.trim();
    let spec = parse_timezone(canonical)?;
    let offset_secs = spec.offset_at(now_utc);
    let offset = UtcOffset::from_whole_seconds(offset_secs)
        .map_err(|e| format!("invalid UTC offset for `{canonical}`: {e}"))?;
    let utc_dt = OffsetDateTime::from_unix_timestamp(now_utc)
        .map_err(|e| format!("invalid timestamp {now_utc}: {e}"))?;
    let local_dt = utc_dt.to_offset(offset);
    let date = local_dt.date();
    let next_date = date
        .next_day()
        .ok_or_else(|| "date overflow computing maintenance window end".to_string())?;
    let window_key = format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        date.month() as u8,
        date.day()
    );
    let window_start_utc = local_midnight_utc(&spec, date)?;
    let window_end_utc = local_midnight_utc(&spec, next_date)?;
    Ok(MaintenanceWindow {
        timezone: canonical.to_string(),
        window_key,
        window_start_utc,
        window_end_utc,
    })
}

fn local_midnight_utc(spec: &TimeZoneSpec, date: Date) -> Result<i64, String> {
    let naive_ts = date.midnight().assume_utc().unix_timestamp();
    let off1 = spec.offset_at(naive_ts);
    let t1 = naive_ts - i64::from(off1);
    let off2 = spec.offset_at(t1);
    let t2 = naive_ts - i64::from(off2);
    if spec.offset_at(t2) == off2 {
        return Ok(t2);
    }
    let off3 = spec.offset_at(t2);
    Ok(naive_ts - i64::from(off3))
}

impl TimeZoneSpec {
    fn offset_at(&self, utc_ts: i64) -> i32 {
        match self {
            Self::Fixed(secs) => *secs,
            Self::Tzif(tz) => tz.offset_at(utc_ts),
        }
    }
}

impl TzifData {
    fn offset_at(&self, utc_ts: i64) -> i32 {
        if self.transitions.is_empty() {
            if let Some(footer) = &self.footer {
                return footer.offset_at(utc_ts);
            }
            return self.ut_offsets.first().copied().unwrap_or(0);
        }
        let idx = self.transitions.partition_point(|&t| t <= utc_ts);
        if idx == 0 {
            return self.ut_offsets.first().copied().unwrap_or(0);
        }
        if idx == self.transitions.len()
            && let Some(footer) = &self.footer
        {
            return footer.offset_at(utc_ts);
        }
        let type_idx = usize::from(self.type_indices[idx - 1]);
        self.ut_offsets.get(type_idx).copied().unwrap_or(0)
    }
}

fn parse_timezone(raw: &str) -> Result<TimeZoneSpec, String> {
    if raw.is_empty() || raw.len() > 128 {
        return Err("invalid timezone name".into());
    }
    if matches!(
        raw.to_ascii_uppercase().as_str(),
        "UTC" | "ETC/UTC" | "GMT" | "ETC/GMT" | "Z"
    ) {
        return Ok(TimeZoneSpec::Fixed(0));
    }
    if let Some(fixed) = parse_fixed_offset(raw) {
        return Ok(TimeZoneSpec::Fixed(fixed));
    }
    let path = resolve_zoneinfo_path(raw)?;
    let bytes = std::fs::read(&path).map_err(|_| format!("unknown timezone `{raw}`"))?;
    let data = parse_tzif(&bytes).map_err(|e| format!("invalid tzdata for `{raw}`: {e}"))?;
    Ok(TimeZoneSpec::Tzif(data))
}

fn parse_fixed_offset(raw: &str) -> Option<i32> {
    let rest = if let Some(r) = raw.strip_prefix("UTC").or_else(|| raw.strip_prefix("utc")) {
        if r.is_empty() {
            return Some(0);
        }
        r
    } else if raw.starts_with('+') || raw.starts_with('-') {
        raw
    } else {
        return None;
    };
    let sign: i32 = match rest.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let body = &rest[1..];
    let (h, m) = if let Some((hh, mm)) = body.split_once(':') {
        if hh.is_empty() || mm.len() != 2 {
            return None;
        }
        (hh.parse::<i32>().ok()?, mm.parse::<i32>().ok()?)
    } else if body.len() == 4 && body.bytes().all(|b| b.is_ascii_digit()) {
        (
            body[..2].parse::<i32>().ok()?,
            body[2..].parse::<i32>().ok()?,
        )
    } else if !body.is_empty() && body.len() <= 2 && body.bytes().all(|b| b.is_ascii_digit()) {
        (body.parse::<i32>().ok()?, 0)
    } else {
        return None;
    };
    if h > 23 || m >= 60 {
        return None;
    }
    Some(sign * (h * 3600 + m * 60))
}

fn resolve_zoneinfo_path(name: &str) -> Result<PathBuf, String> {
    let rel = Path::new(name);
    if rel.is_absolute() || name.contains('\\') || name.contains('\0') {
        return Err(format!("invalid timezone `{name}`"));
    }
    let mut clean = PathBuf::new();
    for comp in rel.components() {
        match comp {
            Component::Normal(seg) => clean.push(seg),
            _ => return Err(format!("invalid timezone `{name}`")),
        }
    }
    if clean.as_os_str().is_empty() {
        return Err("empty timezone".into());
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = std::env::var("TZDIR")
        && !dir.trim().is_empty()
    {
        roots.push(PathBuf::from(dir));
    }
    roots.push(PathBuf::from("/usr/share/zoneinfo"));
    roots.push(PathBuf::from("/var/db/timezone/zoneinfo"));
    for root in roots {
        let candidate = root.join(&clean);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(format!("unknown timezone `{name}`"))
}

fn read_u32(slice: &[u8]) -> Result<u32, String> {
    let arr: [u8; 4] = slice.try_into().map_err(|_| "truncated TZif u32")?;
    Ok(u32::from_be_bytes(arr))
}

fn read_i32(slice: &[u8]) -> Result<i32, String> {
    let arr: [u8; 4] = slice.try_into().map_err(|_| "truncated TZif i32")?;
    Ok(i32::from_be_bytes(arr))
}

fn read_i64(slice: &[u8]) -> Result<i64, String> {
    let arr: [u8; 8] = slice.try_into().map_err(|_| "truncated TZif i64")?;
    Ok(i64::from_be_bytes(arr))
}

struct HeaderCounts {
    version: u8,
    isutcnt: usize,
    isstdcnt: usize,
    leapcnt: usize,
    timecnt: usize,
    typecnt: usize,
    charcnt: usize,
}

fn parse_header(bytes: &[u8], off: usize) -> Result<(HeaderCounts, usize), String> {
    if bytes.len() < off + 44 {
        return Err("truncated TZif header".into());
    }
    if &bytes[off..off + 4] != b"TZif" {
        return Err("missing TZif magic".into());
    }
    let counts = HeaderCounts {
        version: bytes[off + 4],
        isutcnt: read_u32(&bytes[off + 20..off + 24])? as usize,
        isstdcnt: read_u32(&bytes[off + 24..off + 28])? as usize,
        leapcnt: read_u32(&bytes[off + 28..off + 32])? as usize,
        timecnt: read_u32(&bytes[off + 32..off + 36])? as usize,
        typecnt: read_u32(&bytes[off + 36..off + 40])? as usize,
        charcnt: read_u32(&bytes[off + 40..off + 44])? as usize,
    };
    Ok((counts, off + 44))
}

fn parse_tzif(bytes: &[u8]) -> Result<TzifData, String> {
    let (h1, data1_start) = parse_header(bytes, 0)?;
    if matches!(h1.version, b'2' | b'3' | b'4') {
        let skip = h1.timecnt * 4
            + h1.timecnt
            + h1.typecnt * 6
            + h1.charcnt
            + h1.leapcnt * 8
            + h1.isstdcnt
            + h1.isutcnt;
        let h2_off = data1_start + skip;
        let (h2, cursor) = parse_header(bytes, h2_off)?;
        parse_tzif_block(bytes, cursor, &h2, true)
    } else {
        parse_tzif_block(bytes, data1_start, &h1, false)
    }
}

fn parse_tzif_block(
    bytes: &[u8],
    mut cursor: usize,
    hdr: &HeaderCounts,
    wide: bool,
) -> Result<TzifData, String> {
    let t_size = if wide { 8 } else { 4 };
    let mut transitions = Vec::with_capacity(hdr.timecnt);
    for _ in 0..hdr.timecnt {
        let end = cursor + t_size;
        if end > bytes.len() {
            return Err("truncated TZif transitions".into());
        }
        let ts = if wide {
            read_i64(&bytes[cursor..end])?
        } else {
            i64::from(read_i32(&bytes[cursor..end])?)
        };
        transitions.push(ts);
        cursor = end;
    }
    if cursor + hdr.timecnt > bytes.len() {
        return Err("truncated TZif type indices".into());
    }
    let type_indices = bytes[cursor..cursor + hdr.timecnt].to_vec();
    cursor += hdr.timecnt;
    let mut ut_offsets = Vec::with_capacity(hdr.typecnt);
    for _ in 0..hdr.typecnt {
        if cursor + 6 > bytes.len() {
            return Err("truncated TZif ttinfo".into());
        }
        ut_offsets.push(read_i32(&bytes[cursor..cursor + 4])?);
        cursor += 6;
    }
    let leap_entry = if wide { 12 } else { 8 };
    let after_meta = cursor + hdr.charcnt + hdr.leapcnt * leap_entry + hdr.isstdcnt + hdr.isutcnt;
    let footer = if wide && after_meta < bytes.len() {
        let tail = &bytes[after_meta..];
        let trimmed = std::str::from_utf8(tail)
            .ok()
            .and_then(|s| s.trim().strip_prefix('\n').or(Some(s.trim())))
            .map(str::trim)
            .filter(|s| !s.is_empty());
        trimmed.and_then(parse_posix_tz)
    } else {
        None
    };
    Ok(TzifData {
        transitions,
        type_indices,
        ut_offsets,
        footer,
    })
}

fn take_tz_name(s: &str) -> (&str, &str) {
    if let Some(after_open) = s.strip_prefix('<')
        && let Some(end) = after_open.find('>')
    {
        return (&after_open[..end], &after_open[end + 1..]);
    }
    let len = s.bytes().take_while(|b| b.is_ascii_alphabetic()).count();
    (&s[..len], &s[len..])
}

fn take_posix_offset(s: &str) -> Option<(i32, &str)> {
    // POSIX TZ offset sign is inverted vs UTC (e.g. EST5 => UTC-5, CST-8 => UTC+8).
    let (sign, rest) = if let Some(r) = s.strip_prefix('+') {
        (-1, r)
    } else if let Some(r) = s.strip_prefix('-') {
        (1, r)
    } else {
        (-1, s)
    };
    let mut end = 0;
    for b in rest.bytes() {
        if b.is_ascii_digit() || b == b':' {
            end += 1;
        } else {
            break;
        }
    }
    if end == 0 {
        return None;
    }
    let parts: Vec<&str> = rest[..end].split(':').collect();
    let h: i32 = parts.first()?.parse().ok()?;
    let m: i32 = parts.get(1).copied().unwrap_or("0").parse().ok()?;
    let sec: i32 = parts.get(2).copied().unwrap_or("0").parse().ok()?;
    Some((sign * (h * 3600 + m * 60 + sec), &rest[end..]))
}

fn parse_change_rule(s: &str) -> Option<PosixChangeRule> {
    let (date_part, time_part) = match s.split_once('/') {
        Some((d, t)) => (d, Some(t)),
        None => (s, None),
    };
    let body = date_part.strip_prefix('M')?;
    let mut nums = body.split('.');
    let month: u8 = nums.next()?.parse().ok()?;
    let week: u8 = nums.next()?.parse().ok()?;
    let weekday: u8 = nums.next()?.parse().ok()?;
    if !(1..=12).contains(&month)
        || !(1..=5).contains(&week)
        || weekday > 6
        || nums.next().is_some()
    {
        return None;
    }
    let secs_of_day = if let Some(tp) = time_part {
        let parts: Vec<&str> = tp.split(':').collect();
        let h: i32 = parts.first()?.parse().ok()?;
        let m: i32 = parts.get(1).copied().unwrap_or("0").parse().ok()?;
        let sec: i32 = parts.get(2).copied().unwrap_or("0").parse().ok()?;
        h * 3600 + m * 60 + sec
    } else {
        2 * 3600
    };
    Some(PosixChangeRule {
        month,
        week,
        weekday,
        secs_of_day,
    })
}

fn parse_posix_tz(raw: &str) -> Option<PosixTzRule> {
    let (std_name, rest) = take_tz_name(raw);
    if std_name.is_empty() {
        return None;
    }
    let (std_offset, rest) = take_posix_offset(rest)?;
    if rest.is_empty() {
        return Some(PosixTzRule {
            std_offset,
            dst: None,
        });
    }
    let (dst_part, rules) = rest.split_once(',')?;
    let (dst_name, dst_off_part) = take_tz_name(dst_part);
    if dst_name.is_empty() {
        return None;
    }
    let dst_offset = if dst_off_part.is_empty() {
        std_offset + 3600
    } else {
        let (off, rem) = take_posix_offset(dst_off_part)?;
        if !rem.is_empty() {
            return None;
        }
        off
    };
    let (start_s, end_s) = rules.split_once(',')?;
    let start = parse_change_rule(start_s)?;
    let end = parse_change_rule(end_s)?;
    Some(PosixTzRule {
        std_offset,
        dst: Some(PosixDstRule {
            dst_offset,
            start,
            end,
        }),
    })
}

fn transition_utc(year: i32, rule: PosixChangeRule, active_offset: i32) -> i64 {
    let month = Month::try_from(rule.month).unwrap_or(Month::January);
    let mut day_date = Date::from_calendar_date(year, month, 1).unwrap_or(Date::MIN);
    let mut matches = Vec::new();
    while day_date.month() == month {
        if day_date.weekday().number_days_from_sunday() == rule.weekday {
            matches.push(day_date);
        }
        match day_date.next_day() {
            Some(next) => day_date = next,
            None => break,
        }
    }
    let chosen = if rule.week == 5 {
        matches.last().copied().unwrap_or(Date::MIN)
    } else {
        let idx = usize::from(rule.week.saturating_sub(1));
        matches
            .get(idx)
            .copied()
            .or_else(|| matches.last().copied())
            .unwrap_or(Date::MIN)
    };
    chosen.midnight().assume_utc().unix_timestamp() + i64::from(rule.secs_of_day)
        - i64::from(active_offset)
}

impl PosixTzRule {
    fn offset_at(&self, utc_ts: i64) -> i32 {
        let Some(dst) = &self.dst else {
            return self.std_offset;
        };
        let Ok(dt) = OffsetDateTime::from_unix_timestamp(utc_ts) else {
            return self.std_offset;
        };
        let year = dt.year();
        let start_utc = transition_utc(year, dst.start, self.std_offset);
        let end_utc = transition_utc(year, dst.end, dst.dst_offset);
        let in_dst = if start_utc < end_utc {
            utc_ts >= start_utc && utc_ts < end_utc
        } else {
            utc_ts >= start_utc || utc_ts < end_utc
        };
        if in_dst {
            dst.dst_offset
        } else {
            self.std_offset
        }
    }
}
