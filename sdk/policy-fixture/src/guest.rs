use alloc::{string::String, vec, vec::Vec};

use crawler_plugin_sdk::{crawler, exports};

use crawler::plugin::types::{
    ApiError, DownloadPlanPatch, ErrorCode, Event, EventPayload, FilterInput, FilterPayload,
    GateDecision, GateInput, GatePayload, Header, HttpRequest, HttpTarget, Patch, RescorePatch,
    SearchQueryPatch,
};

#[global_allocator]
static ALLOCATOR: dlmalloc::GlobalDlmalloc = dlmalloc::GlobalDlmalloc;

// SAFETY: the C ABI caller supplies two valid readable regions of `len` bytes.
#[unsafe(no_mangle)]
unsafe extern "C" fn memcmp(
    a: *const core::ffi::c_void,
    b: *const core::ffi::c_void,
    len: usize,
) -> i32 {
    let (a, b) = (a.cast::<u8>(), b.cast::<u8>());
    for index in 0..len {
        // Volatile loads prevent LLVM replacing this implementation with itself.
        // SAFETY: the caller's regions cover every index in this loop.
        let (a, b) = unsafe { (a.add(index).read_volatile(), b.add(index).read_volatile()) };
        if a != b {
            return i32::from(a) - i32::from(b);
        }
    }
    0
}

// SAFETY: the canonical ABI guarantees a valid prior allocation and alignment.
#[unsafe(no_mangle)]
unsafe extern "C" fn cabi_realloc(ptr: *mut u8, old: usize, align: usize, new: usize) -> *mut u8 {
    if old == 0 && new == 0 {
        return align as *mut u8;
    }
    let layout = match core::alloc::Layout::from_size_align(old.max(new), align) {
        Ok(layout) => layout,
        Err(_) => core::arch::wasm32::unreachable(),
    };
    // SAFETY: validated layout and canonical ABI ownership of the prior allocation.
    let result = unsafe {
        if old == 0 {
            alloc::alloc::alloc(layout)
        } else {
            let old_layout = core::alloc::Layout::from_size_align_unchecked(old, align);
            alloc::alloc::realloc(ptr, old_layout, new)
        }
    };
    if result.is_null() {
        core::arch::wasm32::unreachable();
    }
    result
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    core::arch::wasm32::unreachable()
}

struct Policy;

fn error(code: ErrorCode) -> ApiError {
    ApiError {
        code,
        message: String::from("fixture"),
        detail: None,
        retry_after_ms: None,
        operation_id: None,
    }
}

impl exports::crawler::plugin::lifecycle::Guest for Policy {
    fn validate(_: String, _: u32) -> Result<(), ApiError> {
        Ok(())
    }
    fn health() -> Result<exports::crawler::plugin::lifecycle::HealthState, ApiError> {
        Ok(exports::crawler::plugin::lifecycle::HealthState::Healthy)
    }
}

impl exports::crawler::plugin::hooks::Guest for Policy {
    fn filter(input: FilterInput) -> Result<Vec<Patch>, ApiError> {
        if input.schema_version != 1 {
            return Err(error(ErrorCode::InvalidArgument));
        }
        match input.payload {
            FilterPayload::SearchBefore(sb) => {
                if sb.query.keyword == "verify-context" {
                    emit_verified_context(&input.context)?;
                }
                if sb.query.keyword == "kv" {
                    crawler::plugin::kv::get("probe")?;
                }
                if sb.query.keyword == "secret-error" {
                    return Err(ApiError {
                        code: ErrorCode::Upstream,
                        message: String::from(
                            "Cookie: uid=1; pass=SECRET_COOKIE_PASS {\"passkey\":\"SECRET_JSON_PK\"}",
                        ),
                        detail: None,
                        retry_after_ms: None,
                        operation_id: None,
                    });
                }
                Ok(vec![Patch::SearchQuery(SearchQueryPatch {
                    keyword: Some(sb.query.keyword),
                    site_ids: Some(sb.site_ids),
                    categories: None,
                })])
            }
            FilterPayload::SearchSiteAfter(ssa) => Ok(ssa
                .candidates
                .into_iter()
                .filter(|c| c.title == "A")
                .map(|c| Patch::Drop(c.handle))
                .collect()),
            FilterPayload::SearchAfter(batch) => filter_search_after(input.context, batch),
            FilterPayload::FilterAfter(fa) => filter_after_patches(fa),
            FilterPayload::SubscribeScore(ss) => {
                let first = ss
                    .candidates
                    .first()
                    .map(|c| c.handle.clone())
                    .unwrap_or_default();
                let delta = if ss.candidates.iter().any(|c| c.title.contains("OverBudget")) {
                    5
                } else {
                    3
                };
                Ok(vec![Patch::Rescore(RescorePatch {
                    handle: first,
                    delta,
                    reason: String::from("subscribe-score"),
                })])
            }
            FilterPayload::SubscribeSelect(sel) => {
                Ok(vec![Patch::SelectHandles(sel.admitted_handles)])
            }
            FilterPayload::DownloadBefore(db) => {
                let target_dl = db
                    .candidate
                    .title
                    .find("RetargetDl:")
                    .and_then(|pos| db.candidate.title[pos + 11..].split_whitespace().next())
                    .map(String::from)
                    .unwrap_or(db.plan.downloader_id);
                Ok(vec![Patch::DownloadPlan(DownloadPlanPatch {
                    downloader_id: Some(target_dl),
                    relative_directory: Some(String::from("plugins/demo")),
                    category: db.plan.category,
                    add_tags: Vec::new(),
                    remove_tags: Vec::new(),
                    paused: Some(db.plan.paused),
                    upload_limit: db.plan.upload_limit,
                    download_limit: db.plan.download_limit,
                    sequential: Some(db.plan.sequential),
                    first_last_piece: Some(db.plan.first_last_piece),
                })])
            }
            FilterPayload::TransferRename(tr) => {
                if tr.source_path.contains("Traversal") || input.context.plugin_id.contains("t2tr1")
                {
                    return Ok(vec![Patch::RenamePath(String::from("../evil_escape.mkv"))]);
                }
                if tr.source_path.contains("Lease.Check")
                    || input.context.plugin_id.contains("t2tr4")
                {
                    return Err(error(ErrorCode::Upstream));
                }
                match tr.media_id.as_str() {
                    "noop" => Ok(Vec::new()),
                    "invalid-path" => Ok(vec![Patch::RenamePath(String::from("../escape.mkv"))]),
                    "chain" => Ok(vec![Patch::RenamePath(alloc::format!(
                        "{}.chained.mkv",
                        tr.proposed_relative_path
                    ))]),
                    "secret-error" => Err(ApiError {
                        code: ErrorCode::Upstream,
                        message: String::from(
                            "Cookie: uid=1; pass=SECRET_COOKIE_PASS {\"passkey\":\"SECRET_JSON_PK\"}",
                        ),
                        detail: None,
                        retry_after_ms: None,
                        operation_id: None,
                    }),
                    "trap" => core::arch::wasm32::unreachable(),
                    "m-1" => Ok(vec![Patch::RenamePath(String::from("Movie/Renamed.mkv"))]),
                    _ => Ok(vec![Patch::RenamePath(alloc::format!(
                        "renamed/{}",
                        tr.proposed_relative_path
                    ))]),
                }
            }
        }
    }
    fn gate(input: GateInput) -> Result<GateDecision, ApiError> {
        if input.schema_version != 1 {
            return Err(error(ErrorCode::InvalidArgument));
        }
        if input.handles.first().map(String::as_str) == Some("verify-context") {
            emit_verified_context(&input.context)?;
        }
        if let GatePayload::DownloadBefore(db) = &input.payload {
            if db.candidate.title.contains("DeferDownload") {
                return Ok(GateDecision::Defer((
                    1_000,
                    String::from("Cookie: uid=1; pass=SECRET_DEFER_PASS"),
                )));
            }
            if db.candidate.title.contains("RejectDownload") {
                return Ok(GateDecision::Reject(String::from("reject-download")));
            }
            if db.candidate.title == "plugin:org.example.dl-defer-kv" {
                if crawler::plugin::kv::get("defer")?.is_some() {
                    return Ok(GateDecision::Defer((1_000, String::from("kv-defer-once"))));
                }
                return Ok(GateDecision::Allow);
            }
        }
        match input.handles.first().map(String::as_str) {
            Some("reject-me") => Ok(GateDecision::Reject(String::from("wasm-reject"))),
            Some("defer-me") => Ok(GateDecision::Defer((9_000, String::from("wasm-defer")))),
            Some("abstain-me") => Ok(GateDecision::Abstain),
            _ => Ok(GateDecision::Allow),
        }
    }
    fn notify(event: Event) -> Result<(), ApiError> {
        if event.schema_version != 1 || event.context.plugin_id.is_empty() {
            return Err(error(ErrorCode::InvalidArgument));
        }
        if event.handles.first().map(String::as_str) == Some("verify-context") {
            emit_verified_context(&event.context)?;
        }
        if event.context.plugin_id.contains("evtwc") {
            crawler::plugin::kv::put("forbid", b"1", None)?;
        }
        match event.id.as_str() {
            "busy" => return Err(error(ErrorCode::Busy)),
            "rate-limit" => {
                let mut err = error(ErrorCode::Upstream);
                err.detail = Some(crawler::plugin::types::ErrorDetail::Upstream(
                    crawler::plugin::types::UpstreamKind::RateLimited,
                ));
                err.retry_after_ms = Some(250);
                return Err(err);
            }
            "invalid-arg" => return Err(error(ErrorCode::InvalidArgument)),
            "trap" => core::arch::wasm32::unreachable(),
            "secret-error" => {
                return Err(ApiError {
                    code: ErrorCode::Upstream,
                    message: String::from(
                        "https://user:SECRET_PASS@pt.example/cb?token=SECRET_TOK",
                    ),
                    detail: None,
                    retry_after_ms: None,
                    operation_id: None,
                });
            }
            "verify-transfer-completed" => match &event.payload {
                EventPayload::TransferCompleted(tc)
                    if event.kind == "transfer.completed"
                        && !event.context.cause_id.is_empty()
                        && !tc.media_id.is_empty()
                        && !tc.library_id.is_empty() =>
                {
                    return Ok(());
                }
                _ => return Err(error(ErrorCode::InvalidArgument)),
            },
            _ => {}
        }
        match &event.payload {
            EventPayload::None
            | EventPayload::DownloadAfter(_)
            | EventPayload::TransferCompleted(_)
            | EventPayload::SiteCheckinAfter(_) => post_event_webhook(&event),
        }
    }
}

fn json_escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    for ch in raw.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&alloc::format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn post_event_webhook(event: &Event) -> Result<(), ApiError> {
    let id_json = json_escape(&event.id);
    let kind_json = json_escape(&event.kind);
    let body_json = alloc::format!(
        "{{\"id\":{id_json},\"event_id\":{id_json},\"kind\":{kind_json},\"body\":{{\"id\":{id_json},\"event_id\":{id_json},\"kind\":{kind_json}}}}}"
    );
    let req = HttpRequest {
        method: String::from("POST"),
        target: HttpTarget::PublicUrl(String::from("/webhook/events")),
        headers: vec![Header {
            name: String::from("content-type"),
            value: String::from("application/json"),
        }],
        body: Some(body_json.into_bytes()),
        max_bytes: 65_536,
    };
    match crawler::plugin::http::fetch(&req) {
        Ok(resp) if resp.status >= 500 || resp.status == 429 => {
            let kind = if resp.status == 429 {
                crawler::plugin::types::UpstreamKind::RateLimited
            } else {
                crawler::plugin::types::UpstreamKind::Network
            };
            let mut err = error(ErrorCode::Upstream);
            err.detail = Some(crawler::plugin::types::ErrorDetail::Upstream(kind));
            Err(err)
        }
        Ok(resp) if resp.status >= 400 => Err(error(ErrorCode::Upstream)),
        Ok(_) => Ok(()),
        Err(err)
            if matches!(
                err.code,
                ErrorCode::PermissionDenied
                    | ErrorCode::InvalidArgument
                    | ErrorCode::NotFound
                    | ErrorCode::Unsupported
            ) =>
        {
            Ok(())
        }
        Err(err) => Err(err),
    }
}

fn filter_after_patches(fa: crawler::plugin::types::FilterAfter) -> Result<Vec<Patch>, ApiError> {
    if fa
        .candidates
        .iter()
        .any(|c| c.title.contains("SetOrderKeep"))
        && fa.candidates.len() >= 2
    {
        let handles: Vec<String> = fa.candidates.iter().map(|c| c.handle.clone()).collect();
        let second = fa.candidates[1].handle.clone();
        return Ok(vec![
            Patch::Rescore(RescorePatch {
                handle: second,
                delta: 10,
                reason: String::from("filter-after-rescore-second"),
            }),
            Patch::SetOrder(handles),
        ]);
    }
    if fa
        .candidates
        .iter()
        .any(|c| c.title.contains("RescoreSecond"))
        && fa.candidates.len() >= 2
    {
        let second = fa.candidates[1].handle.clone();
        return Ok(vec![Patch::Rescore(RescorePatch {
            handle: second,
            delta: 10,
            reason: String::from("filter-after-rescore-second"),
        })]);
    }
    let first = fa.admitted_handles.first().cloned().unwrap_or_default();
    let delta = if fa.candidates.iter().any(|c| c.title.contains("OverBudget")) {
        8
    } else {
        5
    };
    Ok(vec![Patch::Rescore(RescorePatch {
        handle: first,
        delta,
        reason: String::from("filter-after-boost"),
    })])
}

fn emit_verified_context(input_ctx: &crawler::plugin::types::Context) -> Result<(), ApiError> {
    let ctx = crawler::plugin::runtime::current_context();
    if ctx.invocation_id != input_ctx.invocation_id
        || ctx.plugin_id != input_ctx.plugin_id
        || ctx.generation != input_ctx.generation
        || ctx.config_revision != input_ctx.config_revision
        || ctx.deadline_ms != input_ctx.deadline_ms
    {
        return Err(error(ErrorCode::Conflict));
    }
    let _ = crawler::plugin::log::write(
        crawler::plugin::log::Level::Info,
        "current-context",
        &[
            (String::from("invocation_id"), ctx.invocation_id),
            (String::from("plugin_id"), ctx.plugin_id),
            (
                String::from("generation"),
                alloc::format!("{}", ctx.generation),
            ),
            (
                String::from("config_revision"),
                alloc::format!("{}", ctx.config_revision),
            ),
        ],
    );
    Ok(())
}

fn filter_search_after(
    input_ctx: crawler::plugin::types::Context,
    batch: crawler::plugin::types::SearchAfter,
) -> Result<Vec<Patch>, ApiError> {
    let mode = batch
        .candidates
        .first()
        .map(|v| v.title.as_str())
        .unwrap_or("");
    match mode {
        "trap" => core::arch::wasm32::unreachable(),
        "full-view" => return full_view(&batch),
        "guest-error" => {
            let mut failure = error(ErrorCode::InvalidArgument);
            failure.detail = Some(crawler::plugin::types::ErrorDetail::Upstream(
                crawler::plugin::types::UpstreamKind::Parse,
            ));
            failure.retry_after_ms = Some(125);
            failure.operation_id = Some(String::from("guest-reference"));
            return Err(failure);
        }
        "loop" => loop {
            core::hint::spin_loop();
        },
        "memory" => {
            let pages = core::arch::wasm32::memory_grow::<0>(8192);
            if pages == usize::MAX {
                core::arch::wasm32::unreachable();
            }
        }
        "unknown" => {
            return Ok(vec![
                Patch::Drop(batch.candidates[0].handle.clone()),
                Patch::Drop(String::from("forged-unknown")),
            ]);
        }
        "kv" => {
            crawler::plugin::kv::get("probe")?;
        }
        "context" => {
            let context = crawler::plugin::runtime::current_context();
            if context.invocation_id != input_ctx.invocation_id
                || context.deadline_ms != input_ctx.deadline_ms
            {
                return Err(error(ErrorCode::Conflict));
            }
        }
        "state" => {
            // A guest static must start fresh on every invocation.
            static COUNTER: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
            if COUNTER.fetch_add(1, core::sync::atomic::Ordering::Relaxed) != 0 {
                return Err(error(ErrorCode::Conflict));
            }
        }
        _ => {}
    }
    Ok(batch
        .candidates
        .into_iter()
        .filter(|c| c.title == "A")
        .map(|c| Patch::Drop(c.handle))
        .collect())
}

crawler_plugin_sdk::bindings::export!(Policy with_types_in crawler_plugin_sdk::bindings);

fn full_view(batch: &crawler::plugin::types::SearchAfter) -> Result<Vec<Patch>, ApiError> {
    let first = &batch.candidates[0];
    let unknown = &batch.candidates[1];
    let known = first.leechers == Some(3)
        && first.published_at == Some(1234)
        && first.free == Some(false)
        && first.hr == Some(true)
        && first.release.as_deref() == Some("1080p")
        && first.score == Some(-7)
        && first.labels == [String::from("known")]
        && first.revision == 99;
    let unknown = unknown.size.is_none()
        && unknown.seeders.is_none()
        && unknown.leechers.is_none()
        && unknown.published_at.is_none()
        && unknown.free.is_none()
        && unknown.hr.is_none()
        && unknown.release.is_none()
        && unknown.score.is_none()
        && unknown.labels.is_empty();
    let failure = batch.site_failures.first();
    let failure = batch.site_failures.len() == 1
        && failure.is_some_and(|failure| {
            failure.site_id == "failed-site"
                && failure.error.message == "network failure"
                && matches!(failure.error.code, ErrorCode::Upstream)
                && matches!(
                    failure.error.detail,
                    Some(crawler::plugin::types::ErrorDetail::Upstream(
                        crawler::plugin::types::UpstreamKind::Network
                    ))
                )
        });
    if !known || !unknown || !failure {
        return Err(error(ErrorCode::Conflict));
    }
    Ok(vec![Patch::Drop(first.handle.clone())])
}
