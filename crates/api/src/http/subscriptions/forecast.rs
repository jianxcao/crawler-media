use super::*;

/// GET /subscriptions/{id}/release-forecast — 未来 7 天发布预估。
///
/// 数据源：该订阅最近命中的种子（pending）的 upload_time，按星期聚合出
/// 「每个星期几平均命中 N 部」，再映射到接下来 7 天。没有历史时返回空。
pub(crate) async fn release_forecast(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<UserId>,
    Path(id): Path<String>,
) -> Response {
    let subscribe_id = match SubscribeId::from_str(&id) {
        Ok(id) => id,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "subscription.invalid",
                "订阅 id 无效",
            );
        }
    };
    let store = state.store.lock();
    let Some(subscribe) = store.get_subscribe(subscribe_id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "subscription.missing", "订阅不存在");
    };
    if let Err(response) = ensure_subscribe_owner(&store, &subscribe, user_id) {
        return response;
    }
    let pending = store.load_pending(subscribe_id).unwrap_or_default();
    let now = crate::job_loop::unix_now();
    // 星期几 → 命中数（近 30 天）。
    let mut weekday_counts = [0usize; 7];
    let mut samples = 0usize;
    for (_, item) in &pending {
        let Some(time) = item.torrent.upload_time.as_deref() else {
            continue;
        };
        let Some(unix) = parse_upload_time(time) else {
            continue;
        };
        if unix < now - 30 * 86_400 || unix > now {
            continue;
        }
        let weekday = ((unix / 86_400) % 7).rem_euclid(7) as usize;
        weekday_counts[weekday] += 1;
        samples += 1;
    }
    if samples == 0 {
        return ok(json!({ "samples": 0, "days": [] })).into_response();
    }
    // 未来 7 天：每天取对应星期的历史平均（至少 0）。
    let days: Vec<Value> = (1..=7)
        .map(|offset| {
            let unix = now + offset * 86_400;
            let weekday = ((unix / 86_400) % 7).rem_euclid(7) as usize;
            let count = weekday_counts[weekday];
            json!({
                "date": iso_date(unix),
                "weekday": weekday,
                "count": count,
            })
        })
        .collect();
    ok(json!({ "samples": samples, "days": days })).into_response()
}

/// RFC3339 / "YYYY-MM-DD" / 站内时间戳 → unix 秒（尽力解析，失败 None）。
pub(crate) fn parse_upload_time(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    // RFC3339: 2026-09-20T12:00:00Z / +08:00
    if let Some((date, _time)) = raw.split_once('T') {
        if let Some(unix) = parse_date(date) {
            return Some(unix);
        }
    }
    if raw.len() == 10 && raw.as_bytes()[4] == b'-' && raw.as_bytes()[7] == b'-' {
        return parse_date(raw);
    }
    raw.parse::<i64>().ok()
}

fn parse_date(date: &str) -> Option<i64> {
    let mut parts = date.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(civil_to_days(y, m, d) * 86_400)
}

fn civil_to_days(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn iso_date(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
