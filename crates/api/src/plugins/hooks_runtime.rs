mod invoker;

use std::collections::HashSet;

use domain::{Site, SubscribeId, Torrent};
use hooks::{
    Gate, GateFailurePolicy, HookPipeline, HookTrace, Registration, RescoreBudget, sanitize_reason,
};
use plugin_api::{
    ApiError, Candidate, DownloadBefore, DownloadPlanView, ErrorCode, FilterAfter, FilterPayload,
    GateInput, GatePayload, HOOK_WORLD, HookContext, SearchAfter, SearchBefore, SearchQuery,
    SearchSiteAfter, SiteFailure as PluginSiteFailure, SubscribeScore, SubscribeSelect,
};
use plugin_package::PluginManifest;
use store::plugins::PluginLifecycle;
use uuid::Uuid;

pub(crate) use self::invoker::{
    PolicyAuthority, grant_covers_target_scope, hook_grants_cover_context, resolve_hook_invoker,
    resolve_wasm_hook_invoker,
};
pub(crate) use super::hooks_catalog_and_traces::{
    ApiSubscribeHooks, HookTraceBuffers, hook_catalog_items, record_hook_trace_audit,
};
#[doc(hidden)]
pub use super::transfer_rename::{TransferRenameRequest, run_transfer_rename};
use super::wasm_runner::resolve_plugin_trust;
use crate::management::ApiState;

pub(crate) fn base_hook_context(
    site_id: Option<String>,
    subscribe_id: Option<String>,
    deadline_ms: u64,
) -> HookContext {
    HookContext {
        invocation_id: format!("hook-{}", Uuid::new_v4().simple()),
        cause_id: format!("cause-{}", Uuid::new_v4().simple()),
        plugin_id: String::new(),
        generation: 1,
        config_revision: 1,
        subject: "system".to_string(),
        site_id,
        subscribe_id,
        library_id: None,
        deadline_ms,
        cancelled: false,
    }
}

fn resolve_pipeline_authority_mode(
    credential_bound: bool,
    hook_id: &str,
) -> Option<PolicyAuthority> {
    if credential_bound {
        if !matches!(
            hook_id,
            "search.site.after"
                | "download.before"
                | "site.checkin.after"
                | "download.after"
                | "transfer.completed"
        ) {
            return None;
        }
        return Some(PolicyAuthority::PerInvocationSite);
    }
    Some(PolicyAuthority::PerInvocationUnbound)
}

pub(crate) fn build_hook_pipeline(state: &ApiState, hook_id: &str) -> HookPipeline {
    let policy = if hook_id == "download.before" {
        GateFailurePolicy::Closed
    } else {
        GateFailurePolicy::Open
    };
    let timeout_ms = if matches!(hook_id, "download.before" | "transfer.rename") {
        500
    } else {
        200
    };
    let pipeline = HookPipeline::with_failure_policy(policy);
    let Ok(host) = state.plugins.wasm_host() else {
        return pipeline;
    };
    let Ok(plugins) =
        super::PluginService::list_all_plugins(&state.store, Some(PluginLifecycle::Enabled))
    else {
        return pipeline;
    };
    let required_cap = format!("hooks:{hook_id}");
    let mut regs: Vec<(i64, String, String, Registration)> = Vec::new();

    for plugin in plugins {
        if plugin.lifecycle != PluginLifecycle::Enabled || !plugin.desired_enabled {
            continue;
        }
        let Ok(grants) = state.store.get_plugin_grants(&plugin.plugin_id) else {
            continue;
        };
        if !grants.iter().any(|g| g.capability == required_cap) {
            continue;
        }
        let Ok(Some(version)) = state
            .store
            .get_plugin_version(&plugin.plugin_id, &plugin.package_hash)
        else {
            continue;
        };
        if version.manifest_json == "{}" {
            continue;
        }
        let Ok(manifest) = serde_json::from_str::<PluginManifest>(&version.manifest_json) else {
            continue;
        };
        let Some(authority_mode) =
            resolve_pipeline_authority_mode(version.credential_bound, hook_id)
        else {
            continue;
        };
        let trust = resolve_plugin_trust(
            &plugin.plugin_id,
            &version.signature_status,
            &state.admin_trusted_plugins,
        );
        for comp in &manifest.components {
            if comp.world != HOOK_WORLD || !comp.permissions.iter().any(|p| p == &required_cap) {
                continue;
            }
            for hook in &comp.hooks {
                if hook.id != hook_id {
                    continue;
                }
                let Some(invoker) = resolve_hook_invoker(
                    state,
                    host,
                    &plugin,
                    &version,
                    comp,
                    hook_id,
                    authority_mode.clone(),
                    trust,
                    timeout_ms,
                ) else {
                    continue;
                };
                let priority = i32::try_from(hook.priority).unwrap_or(i32::MAX);
                let mut reg = Registration::for_hook(
                    hook_id,
                    priority,
                    plugin.plugin_id.clone(),
                    comp.id.clone(),
                    invoker,
                );
                reg = reg.with_metadata(
                    u64::try_from(plugin.generation).unwrap_or(1),
                    u64::try_from(plugin.config_revision).unwrap_or(1),
                );
                regs.push((
                    hook.priority,
                    plugin.plugin_id.clone(),
                    comp.id.clone(),
                    reg,
                ));
            }
        }
    }
    regs.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    for (_, _, _, reg) in regs {
        pipeline.register(reg);
    }
    pipeline
}

pub(crate) fn torrent_to_candidate(t: &Torrent, handle: String, provider: &str) -> Candidate {
    Candidate {
        handle,
        site_id: t.site_id.to_string(),
        provider: provider.to_string(),
        generation: 1,
        title: t.title.clone(),
        size: t.size_bytes,
        seeders: t.seeders,
        leechers: t.leechers,
        published_at: t.upload_time.as_deref().and_then(parse_upload_time_ms),
        free: Some(t.free),
        hr: t.hr,
        release: release::parse(&t.title).resolution,
        score: None,
        labels: Vec::new(),
        revision: 1,
    }
}

pub(crate) struct SearchBeforeOutcome {
    pub sites: Vec<Site>,
    pub keyword: String,
    pub categories: Vec<String>,
    pub traces: Vec<HookTrace>,
    pub rejected_reason: Option<String>,
}

pub(crate) fn run_search_before(
    state: &ApiState,
    mut sites: Vec<Site>,
    keyword: String,
    page: u32,
    categories: Vec<String>,
) -> SearchBeforeOutcome {
    let pipeline = build_hook_pipeline(state, "search.before");
    let mut query = SearchQuery {
        keyword: keyword.clone(),
        page,
        categories,
    };
    let mut site_ids: Vec<String> = sites.iter().map(|s| s.id.to_string()).collect();
    let ctx = base_hook_context(None, None, 200);
    let workflow_id = format!("search:{}", ctx.invocation_id);
    let mut traces = Vec::new();

    if let Ok(trace) =
        pipeline.filter_search_query_with_trace(&workflow_id, &ctx, &mut query, &mut site_ids)
        && !trace.steps.is_empty()
    {
        record_hook_trace_audit(state, "search.before", &workflow_id, &trace, false);
        traces.push(trace);
    }
    let allowed_sites: HashSet<String> = site_ids.iter().cloned().collect();
    sites.retain(|s| allowed_sites.contains(&s.id.to_string()));

    let mut handles = Vec::new();
    if !query.keyword.is_empty() {
        handles.push(query.keyword.clone());
    }
    let gate_input = GateInput {
        schema_version: 1,
        context: ctx,
        hook_id: "search.before".to_string(),
        handles,
        revision: 1,
        payload: plugin_api::GatePayload::SearchBefore(SearchBefore {
            query: query.clone(),
            site_ids,
        }),
    };
    let mut rejected_reason = None;
    if let Ok((decision, g_trace)) = pipeline.gate_typed_with_trace(&workflow_id, &gate_input) {
        if !g_trace.steps.is_empty() {
            record_hook_trace_audit(state, "search.before", &workflow_id, &g_trace, false);
            traces.push(g_trace);
        }
        match decision {
            Gate::Reject(r) => rejected_reason = Some(sanitize_reason(&r)),
            Gate::Defer { reason, .. } => rejected_reason = Some(sanitize_reason(&reason)),
            Gate::Allow | Gate::Abstain => {}
        }
    }

    SearchBeforeOutcome {
        sites,
        keyword: query.keyword,
        categories: query.categories,
        traces,
        rejected_reason,
    }
}

pub(crate) fn run_search_site_after(
    state: &ApiState,
    site: &Site,
    keyword: &str,
    page: u32,
    categories: &[String],
    mut torrents: Vec<Torrent>,
) -> (Vec<Torrent>, Option<HookTrace>) {
    let pipeline = build_hook_pipeline(state, "search.site.after");
    let mut cands: Vec<Candidate> = torrents
        .iter()
        .enumerate()
        .map(|(i, t)| torrent_to_candidate(t, format!("site:{i}"), &site.profile_id))
        .collect();
    let template = FilterPayload::SearchSiteAfter(SearchSiteAfter {
        site_id: site.id.to_string(),
        query: SearchQuery {
            keyword: keyword.to_string(),
            page,
            categories: categories.to_vec(),
        },
        candidates: cands.clone(),
    });
    let ctx = base_hook_context(Some(site.id.to_string()), None, 200);
    let workflow_id = format!("search:{}", site.id);
    let trace = match pipeline.filter_candidates_with_trace(
        "search.site.after",
        &workflow_id,
        &ctx,
        &mut cands,
        None,
        None,
        &template,
    ) {
        Ok(t) => t,
        Err(_) => return (torrents, None),
    };
    torrents = reorder_by_handles(torrents, &cands, "site:");
    record_hook_trace_audit(state, "search.site.after", &workflow_id, &trace, false);
    let out_trace = (!trace.steps.is_empty()).then_some(trace);
    (torrents, out_trace)
}

pub(crate) fn run_search_after(
    state: &ApiState,
    items: Vec<(Torrent, String)>,
    failures: &[indexer::SiteFailure],
) -> (Vec<(Torrent, String)>, Option<HookTrace>) {
    let pipeline = build_hook_pipeline(state, "search.after");
    let mut cands: Vec<Candidate> = items
        .iter()
        .enumerate()
        .map(|(i, (t, _))| torrent_to_candidate(t, format!("all:{i}"), "indexer"))
        .collect();
    let api_failures: Vec<PluginSiteFailure> = failures
        .iter()
        .map(|f| PluginSiteFailure {
            site_id: f.site_id.to_string(),
            error: ApiError {
                code: ErrorCode::Upstream,
                message: sanitize_reason(&f.error),
                detail: None,
                retry_after_ms: None,
                operation_id: None,
            },
        })
        .collect();
    let template = FilterPayload::SearchAfter(SearchAfter {
        candidates: cands.clone(),
        site_failures: api_failures,
    });
    let ctx = base_hook_context(None, None, 250);
    let workflow_id = format!("search:{}", ctx.invocation_id);
    let trace = match pipeline.filter_candidates_with_trace(
        "search.after",
        &workflow_id,
        &ctx,
        &mut cands,
        None,
        None,
        &template,
    ) {
        Ok(t) => t,
        Err(_) => return (items, None),
    };
    let out = reorder_by_handles(items, &cands, "all:");
    record_hook_trace_audit(state, "search.after", &workflow_id, &trace, false);
    let out_trace = (!trace.steps.is_empty()).then_some(trace);
    (out, out_trace)
}

pub(crate) fn run_filter_after(
    state: &ApiState,
    items: Vec<(Torrent, String)>,
    budget: &mut RescoreBudget,
) -> (Vec<(Torrent, String)>, Option<HookTrace>) {
    let pipeline = build_hook_pipeline(state, "filter.after");
    let mut cands: Vec<Candidate> = items
        .iter()
        .enumerate()
        .map(|(i, (t, _))| torrent_to_candidate(t, format!("flt:{i}"), "indexer"))
        .collect();
    let mut admitted: Vec<String> = cands.iter().map(|c| c.handle.clone()).collect();
    let template = FilterPayload::FilterAfter(FilterAfter {
        candidates: cands.clone(),
        admitted_handles: admitted.clone(),
    });
    let ctx = base_hook_context(None, None, 250);
    let workflow_id = format!("filter:{}", ctx.invocation_id);
    let (trace, set_order_applied) = match pipeline.filter_candidates_with_trace_ordered(
        "filter.after",
        &workflow_id,
        &ctx,
        &mut cands,
        Some(&mut admitted),
        Some(budget),
        &template,
    ) {
        Ok(res) => res,
        Err(_) => return (items, None),
    };
    if !set_order_applied {
        cands.sort_by_key(|a| std::cmp::Reverse(a.score.unwrap_or(0)));
    }
    let out = reorder_by_handles(items, &cands, "flt:");
    record_hook_trace_audit(state, "filter.after", &workflow_id, &trace, false);
    let out_trace = (!trace.steps.is_empty()).then_some(trace);
    (out, out_trace)
}

pub(crate) fn run_subscribe_filter_after(
    state: &ApiState,
    subscribe_id: SubscribeId,
    candidates: &mut Vec<Candidate>,
    admitted: &mut Vec<String>,
    budget: &mut RescoreBudget,
) -> Option<HookTrace> {
    let pipeline = build_hook_pipeline(state, "filter.after");
    let template = FilterPayload::FilterAfter(FilterAfter {
        candidates: candidates.clone(),
        admitted_handles: admitted.clone(),
    });
    let ctx = base_hook_context(None, Some(subscribe_id.to_string()), 250);
    let workflow_id = format!("subscribe:{subscribe_id}");
    let trace = pipeline
        .filter_candidates_with_trace(
            "filter.after",
            &workflow_id,
            &ctx,
            candidates,
            Some(admitted),
            Some(budget),
            &template,
        )
        .ok()?;
    if trace.steps.is_empty() {
        return None;
    }
    record_hook_trace_audit(state, "filter.after", &workflow_id, &trace, false);
    state
        .plugins
        .record_subscribe_trace(subscribe_id, trace.clone());
    Some(trace)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_subscribe_score(
    state: &ApiState,
    subscribe_id: SubscribeId,
    media_id: domain::MediaId,
    candidates: &mut Vec<Candidate>,
    admitted: &mut Vec<String>,
    budget: &mut RescoreBudget,
) -> Option<HookTrace> {
    let pipeline = build_hook_pipeline(state, "subscribe.score");
    let template = FilterPayload::SubscribeScore(SubscribeScore {
        subscribe_id: subscribe_id.to_string(),
        media_id: media_id.to_string(),
        candidates: candidates.clone(),
    });
    let ctx = base_hook_context(None, Some(subscribe_id.to_string()), 250);
    let workflow_id = format!("subscribe:{subscribe_id}");
    let trace = pipeline
        .filter_candidates_with_trace(
            "subscribe.score",
            &workflow_id,
            &ctx,
            candidates,
            Some(admitted),
            Some(budget),
            &template,
        )
        .ok()?;
    if trace.steps.is_empty() {
        return None;
    }
    record_hook_trace_audit(state, "subscribe.score", &workflow_id, &trace, false);
    state
        .plugins
        .record_subscribe_trace(subscribe_id, trace.clone());
    Some(trace)
}

pub(crate) fn run_subscribe_select(
    state: &ApiState,
    subscribe_id: SubscribeId,
    media_id: domain::MediaId,
    candidates: &mut Vec<Candidate>,
    admitted: &mut Vec<String>,
) -> (Gate, Vec<HookTrace>) {
    let pipeline = build_hook_pipeline(state, "subscribe.select");
    let template = FilterPayload::SubscribeSelect(SubscribeSelect {
        subscribe_id: subscribe_id.to_string(),
        media_id: media_id.to_string(),
        candidates: candidates.clone(),
        admitted_handles: admitted.clone(),
    });
    let ctx = base_hook_context(None, Some(subscribe_id.to_string()), 250);
    let workflow = format!("subscribe:{subscribe_id}");
    let mut traces = Vec::new();
    if let Ok(trace) = pipeline.filter_candidates_with_trace(
        "subscribe.select",
        &workflow,
        &ctx,
        candidates,
        Some(admitted),
        None,
        &template,
    ) && !trace.steps.is_empty()
    {
        record_hook_trace_audit(state, "subscribe.select", &workflow, &trace, false);
        state
            .plugins
            .record_subscribe_trace(subscribe_id, trace.clone());
        traces.push(trace);
    }
    let gate_input = GateInput {
        schema_version: 1,
        context: ctx,
        hook_id: "subscribe.select".to_string(),
        handles: admitted.clone(),
        revision: 1,
        payload: GatePayload::SubscribeSelect(SubscribeSelect {
            subscribe_id: subscribe_id.to_string(),
            media_id: media_id.to_string(),
            candidates: candidates.clone(),
            admitted_handles: admitted.clone(),
        }),
    };
    let gate = match pipeline.gate_typed_with_trace(&workflow, &gate_input) {
        Ok((g, gt)) => {
            if !gt.steps.is_empty() {
                record_hook_trace_audit(state, "subscribe.select", &workflow, &gt, false);
                state
                    .plugins
                    .record_subscribe_trace(subscribe_id, gt.clone());
                traces.push(gt);
            }
            g
        }
        Err(_) => Gate::Allow,
    };
    (gate, traces)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_download_before(
    state: &ApiState,
    subscribe_id: Option<SubscribeId>,
    candidate: Candidate,
    purpose: &str,
    mut plan: DownloadPlanView,
    allowed_downloaders: &[String],
) -> (DownloadPlanView, Gate, Vec<HookTrace>) {
    let pipeline = build_hook_pipeline(state, "download.before");
    let sub_str = subscribe_id.map(|s| s.to_string());
    let ctx = base_hook_context(Some(candidate.site_id.clone()), sub_str.clone(), 500);
    let workflow = sub_str.as_deref().map_or_else(
        || format!("download:{}", ctx.invocation_id),
        |s| format!("subscribe:{s}"),
    );
    let mut traces = Vec::new();
    let db_payload = DownloadBefore {
        candidate: candidate.clone(),
        purpose: purpose.to_string(),
        plan: plan.clone(),
    };
    let template = FilterPayload::DownloadBefore(Box::new(db_payload.clone()));
    if let Ok(trace) = pipeline.filter_download_plan_with_trace(
        &workflow,
        &ctx,
        &mut plan,
        allowed_downloaders,
        &template,
    ) && !trace.steps.is_empty()
    {
        record_hook_trace_audit(state, "download.before", &workflow, &trace, false);
        if let Some(id) = subscribe_id {
            state.plugins.record_subscribe_trace(id, trace.clone());
        }
        traces.push(trace);
    }
    let gate_input = GateInput {
        schema_version: 1,
        context: ctx,
        hook_id: "download.before".to_string(),
        handles: vec![candidate.handle.clone()],
        revision: candidate.revision,
        payload: GatePayload::DownloadBefore(Box::new(DownloadBefore {
            candidate,
            purpose: purpose.to_string(),
            plan: plan.clone(),
        })),
    };
    let gate = match pipeline.gate_typed_with_trace(&workflow, &gate_input) {
        Ok((g, gt)) => {
            if !gt.steps.is_empty() {
                record_hook_trace_audit(state, "download.before", &workflow, &gt, false);
                if let Some(id) = subscribe_id {
                    state.plugins.record_subscribe_trace(id, gt.clone());
                }
                traces.push(gt);
            }
            g
        }
        Err(reason) => Gate::Defer {
            until_ms: 60_000,
            reason: sanitize_reason(&reason),
        },
    };
    (plan, gate, traces)
}

fn reorder_by_handles<T>(items: Vec<T>, cands: &[Candidate], prefix: &str) -> Vec<T> {
    let mut slots: Vec<Option<T>> = items.into_iter().map(Some).collect();
    let mut out = Vec::with_capacity(cands.len());
    for cand in cands {
        if let Some(idx_str) = cand.handle.strip_prefix(prefix)
            && let Ok(idx) = idx_str.parse::<usize>()
            && let Some(slot) = slots.get_mut(idx)
            && let Some(item) = slot.take()
        {
            out.push(item);
        }
    }
    out
}

fn parse_upload_time_ms(raw: &str) -> Option<u64> {
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
