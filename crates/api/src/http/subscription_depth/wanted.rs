//! Owned Subscribe detail facts and indexed per-unit rendering, without Store IO.
use super::*;
use std::collections::HashMap;
mod index;
pub(super) use index::PendingUnitIndex;
use index::Unit;

pub(crate) struct WantedSnapshot {
    pending: Vec<(i32, crate::store::PendingDownload)>,
    air_dates: HashMap<u32, Option<String>>,
    history: HashMap<Unit, crate::store::WantedHistory>,
    ledger: Vec<domain::FileCoverage>,
    target_score: i32,
    target_label: String,
}

pub(crate) fn capture(
    store: &crate::Store,
    media: &domain::Media,
    subscribe: &Subscribe,
) -> Result<WantedSnapshot, crate::store::StoreError> {
    let (target_score, target_label) = wash_target(store, subscribe);
    let ledger = store.file_coverage_for_media(media.id)?;
    let air_dates = super::catalog_facts::adopted_episodes(store, subscribe, &ledger)?
        .into_iter()
        .map(|episode| (episode.episode_number, episode.air_date))
        .collect();
    Ok(WantedSnapshot {
        air_dates,
        pending: store.load_pending(subscribe.id)?,
        history: store.load_wanted_history(subscribe.id)?,
        ledger,
        target_score,
        target_label,
    })
}

struct WantedUnitContext<'a> {
    media: &'a domain::Media,
    subscribe: &'a Subscribe,
    facts: &'a subscribe::SubscribeFacts,
    pending: &'a [(i32, crate::store::PendingDownload)],
    pending_index: PendingUnitIndex,
    labels: HashMap<Unit, Option<String>>,
    history: &'a HashMap<Unit, crate::store::WantedHistory>,
    air_dates: &'a HashMap<u32, Option<String>>,
    forecast: super::forecast::ForecastBasis,
    target_score: i32,
    target_label: &'a str,
}

pub(crate) fn render(
    snapshot: WantedSnapshot,
    media: &domain::Media,
    subscribe: &Subscribe,
    facts: &subscribe::SubscribeFacts,
    mut air_dates: HashMap<u32, Option<String>>,
) -> Vec<Value> {
    air_dates.extend(snapshot.air_dates.clone());
    let units = crate::open_coverage::units(
        subscribe,
        media,
        facts,
        &snapshot.pending,
        &snapshot.history,
        air_dates.keys().copied(),
    );
    let context = WantedUnitContext {
        media,
        subscribe,
        facts,
        pending: &snapshot.pending,
        pending_index: PendingUnitIndex::new(&snapshot.pending, &units),
        labels: index::coverage_labels(&snapshot.ledger, subscribe.media_id, &units),
        history: &snapshot.history,
        air_dates: &air_dates,
        forecast: super::forecast::ForecastBasis::new(&snapshot.pending),
        target_score: snapshot.target_score,
        target_label: &snapshot.target_label,
    };
    units
        .into_iter()
        .map(|(season, episode)| wanted_unit_json(season, episode, &context))
        .collect()
}

fn wanted_unit_json(
    season: Option<u32>,
    episode: Option<u32>,
    context: &WantedUnitContext<'_>,
) -> Value {
    let fmt = crate::store::rfc3339_from_secs;
    let fact = context.facts.get(season, episode);
    let pending = context
        .pending_index
        .get((season, episode), context.pending);
    let in_flight = pending.is_some();
    let h = context
        .history
        .get(&(season, episode))
        .cloned()
        .unwrap_or_default();
    let status = if fact.is_some() {
        "imported"
    } else if in_flight || h.grabbed_at.is_some() {
        "grabbed"
    } else {
        "wanted"
    };
    let state = unit_wash_state(context.subscribe, fact, in_flight, context.target_score);
    let current_label = context.labels.get(&(season, episode)).cloned().flatten();
    let upgrade = if status == "wanted" {
        Value::Null
    } else {
        json!({
            "active": state == "upgradable" || state == "in_flight",
            "current_label": current_label,
            "target_label": if context.target_label.is_empty() { Value::Null } else { json!(context.target_label) },
            "search_attempts": h.search_attempts,
            "indeterminate": state == "not_comparable",
        })
    };
    let air_date = match (context.media.kind, episode) {
        (domain::MediaKind::Tv, Some(ep)) => context.air_dates.get(&ep).cloned().flatten(),
        _ => None,
    };
    let release_forecast = air_date
        .as_deref()
        .and_then(|date| context.forecast.for_date(date))
        .unwrap_or(Value::Null);
    let resource_timing = wanted_unit_resource_timing(pending, status);
    let next_search = wanted_unit_next_search(context.subscribe, status, &air_date, &h);
    json!({
        "id": format!("{}-{}-{}", context.subscribe.id, season.unwrap_or(0), episode.unwrap_or(0)),
        "season_number": season.unwrap_or(0) as i64,
        "episode_number": episode.unwrap_or(0) as i64,
        "status": status,
        "air_date": air_date,
        "priority": 0,
        "next_search_at": next_search.map(fmt),
        "search_attempts": h.search_attempts,
        "last_search_at": h.last_search_at.map(fmt),
        "release_forecast": release_forecast,
        "resource_timing": resource_timing,
        "grabbed_at": h.grabbed_at.map(fmt),
        "downloaded_at": Value::Null,
        "imported_at": h.imported_at.map(fmt),
        "info_hash": Value::Null,
        "last_reject_reason": h.last_reject_reason,
        "grab_title": h.grab_title,
        "upgrade": upgrade,
    })
}

fn wanted_unit_resource_timing(
    pending: Option<&crate::store::PendingDownload>,
    status: &str,
) -> Value {
    if status != "grabbed" {
        return Value::Null;
    }
    let fmt = crate::store::rfc3339_from_secs;
    pending
        .map(|p| {
            let publish_unix = p
                .torrent
                .upload_time
                .as_deref()
                .and_then(crate::http::subscriptions::parse_upload_time);
            let submitted = p.submitted_at;
            json!({
                "site_id": p.torrent.site_id.to_string(),
                "torrent_id": p.torrent.id.clone(),
                "publish_time": p.torrent.upload_time,
                "first_seen_at": submitted.map(fmt),
                "submitted_at": submitted.map(fmt),
                "publish_to_seen_seconds": Value::Null,
                "seen_to_submit_seconds": Value::Null,
                "publish_to_submit_seconds": match (publish_unix, submitted) {
                    (Some(publish), Some(submit)) if submit >= publish => {
                        json!(submit - publish)
                    }
                    _ => Value::Null,
                },
                "dry_run": false,
            })
        })
        .unwrap_or(Value::Null)
}

fn wanted_unit_next_search(
    subscribe: &Subscribe,
    status: &str,
    air_date: &Option<String>,
    h: &crate::store::WantedHistory,
) -> Option<i64> {
    let now = crate::job_loop::unix_now();
    if status != "wanted" {
        return None;
    }
    match air_date {
        Some(date_str) => {
            let today_str = crate::store::now_rfc3339();
            let today = today_str.split('T').next().unwrap_or(&today_str);
            if date_str.as_str() > today {
                // 播出日在未来：到期日 = 播出日（按当天 0 点估算 unix 时间戳）
                crate::http::subscriptions::parse_upload_time(date_str)
            } else {
                h.last_search_at
                    .map(|ts| ts + subscribe.search_interval_secs as i64)
                    .or(Some(now))
            }
        }
        None => {
            // 未定档但处于订阅周期搜索中：下次搜索 = 上次搜索 + 搜索间隔
            h.last_search_at
                .map(|ts| ts + subscribe.search_interval_secs as i64)
                .or(Some(now))
        }
    }
}
