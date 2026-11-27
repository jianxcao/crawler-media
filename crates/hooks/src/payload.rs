use std::collections::{HashMap, HashSet};

use domain::Torrent;
use plugin_api::{Candidate, SearchAfter, SiteFailure};

use crate::HookPipeline;

#[derive(Clone, Debug)]
pub struct NativeCandidate {
    pub handle: String,
    pub provider: String,
    pub generation: u64,
    pub revision: u64,
    pub torrent: Torrent,
}

impl NativeCandidate {
    pub fn new(
        handle: impl Into<String>,
        provider: impl Into<String>,
        generation: u64,
        revision: u64,
        torrent: Torrent,
    ) -> Self {
        Self {
            handle: handle.into(),
            provider: provider.into(),
            generation,
            revision,
            torrent,
        }
    }

    fn view(&self) -> Candidate {
        Candidate {
            handle: self.handle.clone(),
            site_id: self.torrent.site_id.to_string(),
            provider: self.provider.clone(),
            generation: self.generation,
            revision: self.revision,
            title: self.torrent.title.clone(),
            size: self.torrent.size_bytes,
            seeders: self.torrent.seeders,
            leechers: self.torrent.leechers,
            published_at: self
                .torrent
                .upload_time
                .as_deref()
                .and_then(parse_candidate_published_at_ms),
            free: Some(self.torrent.free),
            hr: self.torrent.hr,
            release: None,
            score: None,
            labels: Vec::new(),
        }
    }
}

fn offset_datetime_to_ms(date: time::OffsetDateTime) -> Option<u64> {
    let nanos = date.unix_timestamp_nanos();
    (nanos >= 0)
        .then(|| u64::try_from(nanos / 1_000_000).ok())
        .flatten()
}

fn parse_naive_utc_datetime_ms(raw: &str) -> Option<u64> {
    let (date_str, time_str) = raw.split_once(' ').or_else(|| raw.split_once('T'))?;
    let mut date_parts = date_str.trim().split('-');
    let year: i32 = date_parts.next()?.parse().ok()?;
    let month_num: u8 = date_parts.next()?.parse().ok()?;
    let day: u8 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() {
        return None;
    }
    let month = time::Month::try_from(month_num).ok()?;
    let date = time::Date::from_calendar_date(year, month, day).ok()?;

    let mut time_parts = time_str.trim().split(':');
    let hour_s = time_parts.next()?;
    let min_s = time_parts.next()?;
    let sec_s = time_parts.next();
    if time_parts.next().is_some() {
        return None;
    }
    let hour: u8 = hour_s.parse().ok()?;
    let minute: u8 = min_s.parse().ok()?;
    let second: u8 = match sec_s {
        Some(s) => s.parse().ok()?,
        None => 0,
    };
    let tm = time::Time::from_hms(hour, minute, second).ok()?;
    offset_datetime_to_ms(time::PrimitiveDateTime::new(date, tm).assume_utc())
}

pub fn parse_candidate_published_at_ms(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(val) = raw.parse::<u64>() {
        return if val < 100_000_000_000 {
            val.checked_mul(1_000)
        } else {
            Some(val)
        };
    }
    if let Ok(dt) = time::OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339)
    {
        return offset_datetime_to_ms(dt);
    }
    if let Ok(dt) = time::OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc2822)
    {
        return offset_datetime_to_ms(dt);
    }
    parse_naive_utc_datetime_ms(raw)
}

pub struct NativeSearch {
    pub candidates: Vec<NativeCandidate>,
    pub site_failures: Vec<SiteFailure>,
    pub diagnostics: Vec<FilterDiagnostic>,
}

#[derive(Clone, Debug)]
pub struct FilterDiagnostic {
    pub plugin: String,
    pub component: String,
    pub failure: plugin_api::Failure,
}

pub struct SearchOutcome {
    pub candidates: Vec<Candidate>,
    pub site_failures: Vec<SiteFailure>,
    pub diagnostics: Vec<FilterDiagnostic>,
}

impl HookPipeline {
    pub fn filter_native(
        &self,
        candidates: Vec<NativeCandidate>,
        site_failures: Vec<SiteFailure>,
    ) -> Result<NativeSearch, String> {
        let mut seen = HashSet::new();
        if candidates
            .iter()
            .any(|candidate| candidate.handle.is_empty() || !seen.insert(candidate.handle.clone()))
        {
            return Err("invalid candidate handles".into());
        }
        let views = candidates.iter().map(NativeCandidate::view).collect();
        let mut originals: HashMap<_, _> = candidates
            .into_iter()
            .map(|candidate| (candidate.handle.clone(), candidate))
            .collect();
        let output = self.search_after(SearchAfter {
            candidates: views,
            site_failures,
        })?;
        let candidates = output
            .candidates
            .into_iter()
            .filter_map(|view| originals.remove(&view.handle))
            .collect();
        Ok(NativeSearch {
            candidates,
            site_failures: output.site_failures,
            diagnostics: output.diagnostics,
        })
    }
}
