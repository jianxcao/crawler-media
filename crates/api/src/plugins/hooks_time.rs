pub(super) fn parse_upload_time_ms(raw: &str) -> Option<u64> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(n) = s.parse::<u64>() {
        return if n < 100_000_000_000 {
            n.checked_mul(1000)
        } else {
            Some(n)
        };
    }
    if let Ok(st) = httpdate::parse_http_date(s) {
        return st
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|d| u64::try_from(d.as_millis()).ok());
    }
    parse_iso_or_sql_ms(s).or_else(|| parse_rfc2822_ms(s))
}

fn parse_iso_or_sql_ms(s: &str) -> Option<u64> {
    if s.len() < 16 || s.as_bytes().get(4) != Some(&b'-') || s.as_bytes().get(7) != Some(&b'-') {
        return None;
    }
    let sep = *s.as_bytes().get(10)?;
    if !matches!(sep, b'T' | b't' | b' ') {
        return None;
    }
    let year: i32 = s.get(0..4)?.parse().ok()?;
    let month_num: u8 = s.get(5..7)?.parse().ok()?;
    let day: u8 = s.get(8..10)?.parse().ok()?;
    let date =
        time::Date::from_calendar_date(year, time::Month::try_from(month_num).ok()?, day).ok()?;

    let rest = s.get(11..)?.trim();
    let (time_part, tz_part) = split_tz_suffix(rest);
    let (time, millis) = parse_hms_millis(time_part)?;
    let offset = parse_tz_offset(tz_part)?;
    let dt = time::PrimitiveDateTime::new(date, time).assume_offset(offset);
    let base_ms = i128::from(dt.unix_timestamp())
        .checked_mul(1000)?
        .checked_add(i128::from(millis))?;
    u64::try_from(base_ms).ok()
}

fn split_tz_suffix(rest: &str) -> (&str, &str) {
    if let Some(stripped) = rest.strip_suffix(['Z', 'z']) {
        return (stripped.trim_end(), "Z");
    }
    for (idx, b) in rest.bytes().enumerate().skip(5) {
        if b == b'+' || b == b'-' {
            return (rest[..idx].trim_end(), rest[idx..].trim());
        }
    }
    (rest, "")
}

fn parse_hms_millis(s: &str) -> Option<(time::Time, u16)> {
    let (hms, frac) = match s.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (s, None),
    };
    let mut parts = hms.split(':');
    let h: u8 = parts.next()?.parse().ok()?;
    let m: u8 = parts.next()?.parse().ok()?;
    let sec: u8 = match parts.next() {
        Some(v) => v.parse().ok()?,
        None => 0,
    };
    if parts.next().is_some() {
        return None;
    }
    let millis = match frac {
        None => 0,
        Some(f) if f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()) => return None,
        Some(f) => {
            let mut acc = 0u16;
            for i in 0..3 {
                let d = f.as_bytes().get(i).map_or(0, |b| b - b'0');
                acc = acc * 10 + u16::from(d);
            }
            acc
        }
    };
    Some((time::Time::from_hms(h, m, sec).ok()?, millis))
}

fn parse_tz_offset(tz: &str) -> Option<time::UtcOffset> {
    let t = tz.trim();
    if t.is_empty() || matches!(t, "Z" | "z" | "GMT" | "UTC" | "UT") {
        return Some(time::UtcOffset::UTC);
    }
    let sign: i8 = match t.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits = &t[1..];
    let (oh, om): (i8, i8) = if let Some((h, m)) = digits.split_once(':') {
        (h.parse().ok()?, m.parse().ok()?)
    } else if digits.len() == 4 {
        (digits[0..2].parse().ok()?, digits[2..4].parse().ok()?)
    } else {
        return None;
    };
    time::UtcOffset::from_hms(sign * oh, sign * om, 0).ok()
}

fn parse_rfc2822_ms(s: &str) -> Option<u64> {
    let body = match s.split_once(',') {
        Some((_, after)) => after.trim(),
        None => s,
    };
    let mut parts = body.split_whitespace();
    let day: u8 = parts.next()?.parse().ok()?;
    let mon = match parts.next()?.to_ascii_lowercase().as_str() {
        "jan" => time::Month::January,
        "feb" => time::Month::February,
        "mar" => time::Month::March,
        "apr" => time::Month::April,
        "may" => time::Month::May,
        "jun" => time::Month::June,
        "jul" => time::Month::July,
        "aug" => time::Month::August,
        "sep" => time::Month::September,
        "oct" => time::Month::October,
        "nov" => time::Month::November,
        "dec" => time::Month::December,
        _ => return None,
    };
    let year: i32 = parts.next()?.parse().ok()?;
    let (time, _) = parse_hms_millis(parts.next()?)?;
    let offset = parse_tz_offset(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    let date = time::Date::from_calendar_date(year, mon, day).ok()?;
    let secs = time::PrimitiveDateTime::new(date, time)
        .assume_offset(offset)
        .unix_timestamp();
    u64::try_from(secs).ok()?.checked_mul(1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_upload_time_supports_epoch_rfc3339_rfc2822_and_sql_timestamps() {
        assert_eq!(parse_upload_time_ms("1700000000"), Some(1_700_000_000_000));
        assert_eq!(
            parse_upload_time_ms("1700000000123"),
            Some(1_700_000_000_123)
        );
        assert_eq!(
            parse_upload_time_ms("2024-03-01T12:00:00.250Z"),
            Some(1_709_294_400_250)
        );
        assert_eq!(
            parse_upload_time_ms("2024-03-01T20:00:00+08:00"),
            Some(1_709_294_400_000)
        );
        assert_eq!(
            parse_upload_time_ms("Fri, 01 Mar 2024 20:00:00 +0800"),
            Some(1_709_294_400_000)
        );
        assert_eq!(
            parse_upload_time_ms("2024-03-01 12:00:00"),
            Some(1_709_294_400_000)
        );
        assert_eq!(
            parse_upload_time_ms("2024-03-01 12:00"),
            Some(1_709_294_400_000)
        );
    }
}
