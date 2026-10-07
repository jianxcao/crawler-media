use super::*;

/// 为有播出日期的单元生成前端兼容的发布预测快照（version 1）。
/// 数据源：pending 各站 upload_time 的星期分布（30 天窗口）。
/// samples 不足或目标日期已过 → None（前端不展示）。
pub(super) fn forecast_for_unit(
    _subscribe: &Subscribe,
    pending: &[(i32, crate::store::PendingDownload)],
    air_date: &str,
) -> Option<Value> {
    let now = crate::job_loop::unix_now();
    let target_unix = crate::http::subscriptions::parse_upload_time(air_date)?;
    if target_unix < now {
        return None;
    }
    let mut weekday_counts = [0usize; 7];
    let mut sites: Vec<(String, u32)> = Vec::new();
    let mut samples = 0usize;
    for (_, item) in pending {
        let Some(time) = item.torrent.upload_time.as_deref() else {
            continue;
        };
        let Some(unix) = crate::http::subscriptions::parse_upload_time(time) else {
            continue;
        };
        if unix < now - 30 * 86_400 || unix > now {
            continue;
        }
        let weekday = ((unix / 86_400) % 7).rem_euclid(7) as usize;
        weekday_counts[weekday] += 1;
        samples += 1;
        if let Some((_site_id, count)) = sites
            .iter_mut()
            .find(|(id, _)| id == &item.torrent.site_id.to_string())
        {
            *count += 1;
        } else {
            sites.push((item.torrent.site_id.to_string(), 1));
        }
    }
    let fmt = crate::store::rfc3339_from_secs;
    let confidence = if samples == 0 {
        // 无历史样本：仍给出 air_date 当天的基础预测（bootstrap，前端可显示「预计入库」）。
        return Some(json!({
            "version": 1,
            "generated_at": fmt(now),
            "target_air_date": air_date,
            "predicted_at": fmt(target_unix),
            "window_start": fmt(target_unix - 86_400),
            "window_end": fmt(target_unix + 86_400),
            "confidence": "bootstrap",
            "sample_count": 0,
            "cadence_days": 7,
            "basis_units": [],
            "basis_torrent_row_ids": [],
            "sites": [],
            "first": {
                "generated_at": fmt(now),
                "predicted_at": fmt(target_unix),
                "window_start": fmt(target_unix - 86_400),
                "window_end": fmt(target_unix + 86_400),
                "confidence": "bootstrap",
                "sample_count": 0,
            },
        }));
    } else if samples < 10 {
        "growing"
    } else {
        "stable"
    };
    let window_start = target_unix - 86_400;
    let window_end = target_unix + 2 * 86_400;
    let basis_units: Vec<Value> = weekday_counts
        .iter()
        .enumerate()
        .map(|(weekday, count)| json!([weekday, count]))
        .collect();
    let site_rows: Vec<Value> = sites
        .into_iter()
        .map(|(site_id, coverage_count)| {
            json!({
                "site_id": site_id,
                "predicted_at": fmt(target_unix),
                "window_start": fmt(window_start),
                "window_end": fmt(window_end),
                "lag_minutes": 0,
                "coverage_count": coverage_count,
                "probe_times": [],
            })
        })
        .collect();
    Some(json!({
        "version": 1,
        "generated_at": fmt(now),
        "target_air_date": air_date,
        "predicted_at": fmt(target_unix),
        "window_start": fmt(window_start),
        "window_end": fmt(window_end),
        "confidence": confidence,
        "sample_count": samples,
        "cadence_days": 7,
        "basis_units": basis_units,
        "basis_torrent_row_ids": [],
        "sites": site_rows,
        "first": {
            "generated_at": fmt(now),
            "predicted_at": fmt(target_unix),
            "window_start": fmt(window_start),
            "window_end": fmt(window_end),
            "confidence": confidence,
            "sample_count": samples,
        },
    }))
}
