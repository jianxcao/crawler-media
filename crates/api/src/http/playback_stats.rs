//! Playback statistics assembled from persisted playback logs.

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

use crate::http::ok;
use crate::http::playback::media_target_json;
use crate::http::playback_logs::{media_visible, member_filter, member_name, visible_media_ids};
use crate::job_loop::unix_now;
use crate::management::ApiState;
use crate::store::PlayLogRow;

/// GET /playback/stats/watch?days&tz_offset&member_id&scope
pub(crate) async fn watch_stats(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let days = query
        .get("days")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(30)
        .clamp(1, 365);
    let tz_minutes = query
        .get("tz_offset")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let now = unix_now();
    let store = state.store.lock();
    let (logs, previous_logs, hidden_count) = load_stats_logs(&store, user_id, &query, days, now);
    ok(stats_json(
        &store,
        &logs,
        &previous_logs,
        hidden_count,
        days,
        tz_minutes,
        now,
    ))
    .into_response()
}

fn load_stats_logs(
    store: &crate::Store,
    user_id: domain::UserId,
    query: &HashMap<String, String>,
    days: i64,
    now: i64,
) -> (Vec<PlayLogRow>, Vec<PlayLogRow>, usize) {
    let admin = store
        .user_role(user_id)
        .map(|role| role == "admin")
        .unwrap_or(false);
    let target_user = if admin {
        member_filter(store, query.get("member_id").map(String::as_str))
    } else {
        Some(user_id)
    };
    let scope = if admin && query.get("scope").is_some_and(|scope| scope == "all") {
        "all"
    } else {
        "visible"
    };
    let visible_media = visible_media_ids(store, user_id, scope);
    let all_logs = store
        .list_logs(200_000, None, None, Some(days), now, target_user)
        .unwrap_or_default();
    let hidden_titles: HashSet<domain::MediaId> = all_logs
        .iter()
        .filter(|log| !media_visible(visible_media.as_ref(), log.media_id))
        .map(|log| log.media_id)
        .collect();
    let logs: Vec<PlayLogRow> = all_logs
        .into_iter()
        .filter(|log| media_visible(visible_media.as_ref(), log.media_id))
        .collect();
    let previous_logs = store
        .list_logs(200_000, None, None, Some(days * 2), now, target_user)
        .unwrap_or_default()
        .into_iter()
        .filter(|log| media_visible(visible_media.as_ref(), log.media_id))
        .collect();
    (logs, previous_logs, hidden_titles.len())
}

fn group_rows(
    store: &crate::Store,
    logs: &[PlayLogRow],
    tz_minutes: i64,
) -> (Vec<Vec<i64>>, Vec<Value>, Vec<Value>) {
    let mut by_hour = vec![vec![0i64; 24]; 7];
    let mut by_member: HashMap<domain::UserId, (i64, i64, i64)> = HashMap::new();
    let mut by_client: HashMap<String, (i64, i64)> = HashMap::new();
    for log in logs {
        let shifted = log.started_at + tz_minutes * 60;
        let hour = (shifted.rem_euclid(86_400) / 3600) as usize;
        let weekday = (shifted.div_euclid(86_400) + 3).rem_euclid(7) as usize;
        by_hour[weekday][hour] += log.watched_ms;
        let m = by_member.entry(log.user_id).or_insert((0, 0, 0));
        m.0 += 1;
        m.1 += log.watched_ms;
        m.2 += log.completed as i64;
        let c = by_client
            .entry(log.client.clone().unwrap_or_else(|| "直连".into()))
            .or_insert((0, 0));
        c.0 += 1;
        c.1 += log.watched_ms;
    }
    let member_rows: Vec<Value> = by_member
        .into_iter()
        .map(|(id, (plays, watched, completed))| {
            json!({
                "member_id": id.to_string(), "member_name": member_name(store, id),
                "plays": plays, "watched_ms": watched, "completed": completed,
            })
        })
        .collect();
    let client_rows: Vec<Value> = by_client
        .into_iter()
        .map(|(client, (plays, watched))| json!({ "client": client, "plays": plays, "watched_ms": watched }))
        .collect();
    (by_hour, member_rows, client_rows)
}

fn stats_json(
    store: &crate::Store,
    logs: &[PlayLogRow],
    previous_logs: &[PlayLogRow],
    hidden_count: usize,
    days: i64,
    tz_minutes: i64,
    now: i64,
) -> Value {
    let prev_window = now - days * 86_400;
    let prev: Vec<&PlayLogRow> = previous_logs
        .iter()
        .filter(|log| log.started_at < prev_window)
        .collect();
    let current_total = json!({
        "plays": logs.len() as i64,
        "watched_ms": logs.iter().map(|l| l.watched_ms).sum::<i64>(),
        "completed": logs.iter().filter(|l| l.completed).count() as i64,
        "active_members": logs.iter().map(|log| log.user_id).collect::<HashSet<_>>().len() as i64,
    });
    let previous_total = json!({
        "plays": prev.len() as i64,
        "watched_ms": prev.iter().map(|l| l.watched_ms).sum::<i64>(),
        "completed": prev.iter().filter(|l| l.completed).count() as i64,
        "active_members": prev.iter().map(|log| log.user_id).collect::<HashSet<_>>().len() as i64,
    });
    let today = (now + tz_minutes * 60).div_euclid(86_400);
    let day_rows = summarize_days(logs.iter(), tz_minutes, today - days + 1, days);
    let previous_by_day =
        summarize_days(prev.iter().copied(), tz_minutes, today - 2 * days + 1, days);
    let (by_hour, member_rows, client_rows) = group_rows(store, logs, tz_minutes);
    let top_titles = summarize_titles(store, logs.iter(), false, 8);
    let favorites = summarize_titles(store, logs.iter(), true, 3);
    let previous_favorites = summarize_titles(store, prev.iter().copied(), true, 3);
    json!({
        "days": days,
        "current": current_total,
        "previous": previous_total,
        "previous_available": !prev.is_empty(),
        "by_day": day_rows,
        "previous_by_day": previous_by_day,
        "by_hour": by_hour,
        "by_member": member_rows,
        "by_client": client_rows,
        "by_tier": [ { "tier": 0, "label": "直连", "plays": logs.len() as i64 } ],
        "top_titles": top_titles,
        "hidden_title_count": hidden_count,
        "favorites": favorites,
        "previous_favorites": previous_favorites,
    })
}

fn summarize_days<'a>(
    logs: impl IntoIterator<Item = &'a PlayLogRow>,
    tz_minutes: i64,
    first_day: i64,
    days: i64,
) -> Vec<Value> {
    let mut totals: HashMap<i64, (i64, i64, i64, HashSet<domain::UserId>)> = HashMap::new();
    for log in logs {
        let day = (log.started_at + tz_minutes * 60).div_euclid(86_400);
        let item = totals.entry(day).or_default();
        item.0 += 1;
        item.1 += log.watched_ms;
        item.2 += i64::from(log.completed);
        item.3.insert(log.user_id);
    }
    (first_day..first_day + days)
        .map(|day| {
            let (plays, watched, completed, members) = totals.remove(&day).unwrap_or_default();
            json!({
                "date": iso_date(day * 86_400),
                "plays": plays, "watched_ms": watched,
                "completed": completed, "members": members.len(),
            })
        })
        .collect()
}

#[derive(Default)]
struct TitleTotals {
    plays: i64,
    watched: i64,
    members: HashSet<domain::UserId>,
}

fn summarize_titles<'a>(
    store: &crate::Store,
    logs: impl IntoIterator<Item = &'a PlayLogRow>,
    by_members: bool,
    limit: usize,
) -> Vec<Value> {
    let mut totals: HashMap<domain::MediaId, TitleTotals> = HashMap::new();
    for log in logs {
        let item = totals.entry(log.media_id).or_default();
        item.plays += 1;
        item.watched += log.watched_ms;
        item.members.insert(log.user_id);
    }
    let mut rows: Vec<_> = totals.into_iter().collect();
    rows.sort_by_key(|(_, total)| {
        std::cmp::Reverse(if by_members {
            (total.members.len() as i64, total.watched)
        } else {
            (total.watched, total.members.len() as i64)
        })
    });
    rows.into_iter()
        .take(limit)
        .map(|(id, total)| {
            json!({
                "media": media_target_json(store, id, None, None),
                "plays": total.plays, "watched_ms": total.watched,
                "members": total.members.len(),
            })
        })
        .collect()
}

fn iso_date(unix_secs: i64) -> String {
    // 按浏览器时区偏移后的日期（YYYY-MM-DD），不做绝对时间换算。
    let days = unix_secs.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days → (year, month, day) in the shifted timezone (Howard Hinnant's algo).
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
