use std::path::{Component, Path, PathBuf};

use time::format_description::well_known::Rfc3339;
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

const DEFAULT_FALLBACK_OFFSET_SECS: i32 = 8 * 3600;

/// Parse an RFC3339 or site-local `"YYYY-MM-DD HH:MM(:SS)"` timestamp into UTC unix seconds.
///
/// Interprets timestamps without an explicit UTC offset in `site_timezone` (with fixed offsets,
/// system zoneinfo TZif lookup, and `+08:00` fallback when an IANA zone cannot be resolved).
pub fn parse_site_datetime(raw: &str, site_timezone: &str) -> Option<i64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(dt) = OffsetDateTime::parse(trimmed, &Rfc3339) {
        return Some(dt.unix_timestamp());
    }
    let pdt = parse_naive_datetime(trimmed)?;
    let offset_secs = resolve_offset_secs(site_timezone, pdt.assume_utc().unix_timestamp());
    let offset = UtcOffset::from_whole_seconds(offset_secs).ok()?;
    Some(pdt.assume_offset(offset).unix_timestamp())
}

fn parse_naive_datetime(raw: &str) -> Option<PrimitiveDateTime> {
    let normalized = raw.replace('T', " ");
    let mut parts = normalized.split_whitespace();
    let date_str = parts.next()?;
    let time_str = parts.next()?;
    let date = parse_ymd(date_str)?;
    let time = parse_hms(time_str)?;
    Some(PrimitiveDateTime::new(date, time))
}

fn parse_ymd(raw: &str) -> Option<Date> {
    let mut segs = raw.split('-');
    let y: i32 = segs.next()?.parse().ok()?;
    let m: u8 = segs.next()?.parse().ok()?;
    let d: u8 = segs.next()?.parse().ok()?;
    if segs.next().is_some() {
        return None;
    }
    let month = Month::try_from(m).ok()?;
    Date::from_calendar_date(y, month, d).ok()
}

fn parse_hms(raw: &str) -> Option<Time> {
    let clean = raw.split(['.', 'Z', '+', '-']).next().unwrap_or(raw).trim();
    let mut segs = clean.split(':');
    let h: u8 = segs.next()?.parse().ok()?;
    let m: u8 = segs.next()?.parse().ok()?;
    let s: u8 = segs.next().unwrap_or("0").parse().ok()?;
    if segs.next().is_some() {
        return None;
    }
    Time::from_hms(h, m, s).ok()
}

pub fn resolve_offset_secs(timezone: &str, approx_utc: i64) -> i32 {
    let tz = timezone.trim();
    if tz.is_empty() {
        return DEFAULT_FALLBACK_OFFSET_SECS;
    }
    if matches!(
        tz.to_ascii_uppercase().as_str(),
        "UTC" | "ETC/UTC" | "GMT" | "ETC/GMT" | "Z"
    ) {
        return 0;
    }
    if let Some(secs) = parse_fixed_offset(tz) {
        return secs;
    }
    if let Some(secs) = lookup_tzif_offset(tz, approx_utc) {
        return secs;
    }
    match tz {
        "Asia/Shanghai" | "Asia/Hong_Kong" | "Asia/Taipei" | "Asia/Chongqing"
        | "Asia/Singapore" | "PRC" => 8 * 3600,
        "Asia/Tokyo" | "Asia/Seoul" => 9 * 3600,
        _ => DEFAULT_FALLBACK_OFFSET_SECS,
    }
}

fn parse_fixed_offset(raw: &str) -> Option<i32> {
    let rest = raw
        .strip_prefix("UTC")
        .or_else(|| raw.strip_prefix("utc"))
        .unwrap_or(raw);
    let sign = match rest.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let body = &rest[1..];
    let (h, m) = if let Some((hh, mm)) = body.split_once(':') {
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
    (h <= 23 && m < 60).then_some(sign * (h * 3600 + m * 60))
}

fn lookup_tzif_offset(name: &str, approx_utc: i64) -> Option<i32> {
    let path = resolve_zoneinfo_path(name)?;
    let bytes = std::fs::read(path).ok()?;
    parse_tzif_offset_at(&bytes, approx_utc)
}

fn resolve_zoneinfo_path(name: &str) -> Option<PathBuf> {
    let rel = Path::new(name);
    if rel.is_absolute() || name.contains('\\') || name.contains('\0') {
        return None;
    }
    let mut clean = PathBuf::new();
    for comp in rel.components() {
        match comp {
            Component::Normal(seg) => clean.push(seg),
            _ => return None,
        }
    }
    if clean.as_os_str().is_empty() {
        return None;
    }
    let mut roots = Vec::new();
    if let Ok(dir) = std::env::var("TZDIR")
        && !dir.trim().is_empty()
    {
        roots.push(PathBuf::from(dir));
    }
    roots.push(PathBuf::from("/usr/share/zoneinfo"));
    roots.push(PathBuf::from("/var/db/timezone/zoneinfo"));
    roots
        .into_iter()
        .map(|root| root.join(&clean))
        .find(|p| p.is_file())
}

fn parse_tzif_offset_at(bytes: &[u8], utc_ts: i64) -> Option<i32> {
    if bytes.len() < 44 || &bytes[..4] != b"TZif" {
        return None;
    }
    let v1_off = skip_tzif_v1(bytes)?;
    let (off, time_size, trans_cnt, type_cnt) = if bytes[4] >= b'2' && bytes.len() >= v1_off + 44 {
        let tc = read_u32(bytes.get(v1_off + 32..v1_off + 36)?)? as usize;
        let tyc = read_u32(bytes.get(v1_off + 36..v1_off + 40)?)? as usize;
        (v1_off, 8usize, tc, tyc)
    } else {
        let tc = read_u32(bytes.get(32..36)?)? as usize;
        let tyc = read_u32(bytes.get(36..40)?)? as usize;
        (0usize, 4usize, tc, tyc)
    };
    if type_cnt == 0 {
        return None;
    }
    let trans_start = off + 44;
    let idx_start = trans_start + trans_cnt * time_size;
    let types_start = idx_start + trans_cnt;
    let mut chosen_type = 0usize;
    if trans_cnt > 0 {
        let mut last_idx = 0usize;
        for i in 0..trans_cnt {
            let t_slice =
                bytes.get(trans_start + i * time_size..trans_start + (i + 1) * time_size)?;
            let trans = if time_size == 8 {
                i64::from_be_bytes(t_slice.try_into().ok()?)
            } else {
                i64::from(i32::from_be_bytes(t_slice.try_into().ok()?))
            };
            if trans <= utc_ts {
                last_idx = i + 1;
            } else {
                break;
            }
        }
        if last_idx > 0 {
            chosen_type = usize::from(*bytes.get(idx_start + last_idx - 1)?);
        }
    }
    let off_start = types_start + chosen_type * 6;
    let ut_off = i32::from_be_bytes(bytes.get(off_start..off_start + 4)?.try_into().ok()?);
    Some(ut_off)
}

fn skip_tzif_v1(bytes: &[u8]) -> Option<usize> {
    let isut = read_u32(bytes.get(20..24)?)? as usize;
    let isstd = read_u32(bytes.get(24..28)?)? as usize;
    let leap = read_u32(bytes.get(28..32)?)? as usize;
    let timecnt = read_u32(bytes.get(32..36)?)? as usize;
    let typecnt = read_u32(bytes.get(36..40)?)? as usize;
    let charcnt = read_u32(bytes.get(40..44)?)? as usize;
    Some(44 + timecnt * 5 + typecnt * 6 + charcnt + leap * 8 + isstd + isut)
}

fn read_u32(slice: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(slice.try_into().ok()?))
}
