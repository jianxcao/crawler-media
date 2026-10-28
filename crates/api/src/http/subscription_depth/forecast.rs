use super::*;
use std::collections::HashMap;

/// One request's pending history, reused for every unit's forecast.
pub(super) struct ForecastBasis {
    now: i64,
    weekday_counts: [usize; 7],
    sites: Vec<(String, u32)>,
    samples: usize,
}

impl ForecastBasis {
    pub(super) fn new(pending: &[(i32, crate::store::PendingDownload)]) -> Self {
        let now = crate::job_loop::unix_now();
        let mut basis = Self {
            now,
            weekday_counts: [0; 7],
            sites: Vec::new(),
            samples: 0,
        };
        let mut positions = HashMap::new();
        for (_, item) in pending {
            let Some(unix) = item
                .torrent
                .upload_time
                .as_deref()
                .and_then(crate::http::subscriptions::parse_upload_time)
            else {
                continue;
            };
            if unix < now - 30 * 86_400 || unix > now {
                continue;
            }
            basis.weekday_counts[((unix / 86_400) % 7).rem_euclid(7) as usize] += 1;
            basis.samples += 1;
            let site = item.torrent.site_id.to_string();
            let position = *positions.entry(site.clone()).or_insert_with(|| {
                basis.sites.push((site, 0));
                basis.sites.len() - 1
            });
            basis.sites[position].1 += 1;
        }
        basis
    }

    pub(super) fn for_date(&self, air_date: &str) -> Option<Value> {
        let target = crate::http::subscriptions::parse_upload_time(air_date)?;
        if target < self.now {
            return None;
        }
        let confidence = if self.samples == 0 {
            "bootstrap"
        } else if self.samples < 10 {
            "growing"
        } else {
            "stable"
        };
        let fmt = crate::store::rfc3339_from_secs;
        let end = target
            + if self.samples == 0 {
                86_400
            } else {
                2 * 86_400
            };
        let basis: Vec<Value> = if self.samples == 0 {
            Vec::new()
        } else {
            self.weekday_counts
                .iter()
                .enumerate()
                .map(|(day, count)| json!([day, count]))
                .collect()
        };
        let sites: Vec<Value> = self.sites.iter().map(|(site,count)| json!({
            "site_id":site, "predicted_at":fmt(target), "window_start":fmt(target-86_400),
            "window_end":fmt(end), "lag_minutes":0, "coverage_count":count, "probe_times":[],
        })).collect();
        Some(json!({
            "version":1, "generated_at":fmt(self.now), "target_air_date":air_date,
            "predicted_at":fmt(target), "window_start":fmt(target-86_400), "window_end":fmt(end),
            "confidence":confidence, "sample_count":self.samples, "cadence_days":7,
            "basis_units":basis, "basis_torrent_row_ids":[], "sites":sites,
            "first":{"generated_at":fmt(self.now), "predicted_at":fmt(target),
                "window_start":fmt(target-86_400), "window_end":fmt(end),
                "confidence":confidence, "sample_count":self.samples},
        }))
    }
}
