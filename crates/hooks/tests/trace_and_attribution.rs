use std::sync::{Arc, Mutex};

use hooks::{
    Gate, GateFailurePolicy, HookPipeline, HookStepStatus, PluginInvoker, Registration,
    RescoreBudget, sanitize_reason,
};
use plugin_api::{
    Candidate, DownloadBefore, DownloadPlanPatch, DownloadPlanView, Event, EventPayload, Failure,
    FilterAfter, FilterInput, FilterPayload, GateInput, GatePayload, HookContext, Patch,
    RescorePatch, SubscribeScore,
};

fn base_context() -> HookContext {
    HookContext {
        invocation_id: "workflow-inv-1".into(),
        cause_id: "cause-1".into(),
        plugin_id: "host".into(),
        generation: 0,
        config_revision: 0,
        subject: "subject:1".into(),
        site_id: None,
        subscribe_id: None,
        library_id: None,
        deadline_ms: 9_999_999,
        cancelled: false,
    }
}

fn candidate(handle: &str, score: i32) -> Candidate {
    Candidate {
        handle: handle.into(),
        site_id: "site-1".into(),
        provider: "prov-1".into(),
        generation: 1,
        title: format!("Title {handle}"),
        size: Some(1_000),
        seeders: Some(5),
        leechers: Some(1),
        published_at: Some(100),
        free: Some(true),
        hr: Some(false),
        release: Some("1080p".into()),
        score: Some(score),
        labels: vec![],
        revision: 1,
    }
}

struct RecordingFilter {
    patches: Vec<Patch>,
    seen_contexts: Arc<Mutex<Vec<HookContext>>>,
}

impl PluginInvoker for RecordingFilter {
    fn filter_typed(&self, input: &FilterInput) -> Result<Vec<Patch>, Failure> {
        self.seen_contexts
            .lock()
            .unwrap()
            .push(input.context.clone());
        Ok(self.patches.clone())
    }
}

struct FailingGate {
    result: Result<Gate, String>,
    seen_contexts: Arc<Mutex<Vec<HookContext>>>,
}

impl PluginInvoker for FailingGate {
    fn gate_typed(&self, input: &GateInput) -> Result<Gate, String> {
        self.seen_contexts
            .lock()
            .unwrap()
            .push(input.context.clone());
        self.result.clone()
    }
}

struct RecordingNotify {
    result: Result<(), String>,
    calls: Arc<Mutex<Vec<(String, HookContext)>>>,
    label: String,
}

impl PluginInvoker for RecordingNotify {
    fn notify_typed(&self, event: &Event) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap()
            .push((self.label.clone(), event.context.clone()));
        self.result.clone()
    }
}

fn register_recording_filter(
    pipeline: &HookPipeline,
    hook_id: &str,
    priority: i32,
    (plugin, comp, generation, cfg_rev): (&str, &str, u64, u64),
    patches: Vec<Patch>,
) -> Arc<Mutex<Vec<HookContext>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    pipeline.register(
        Registration::for_hook(
            hook_id,
            priority,
            plugin,
            comp,
            Arc::new(RecordingFilter {
                patches,
                seen_contexts: Arc::clone(&seen),
            }),
        )
        .with_metadata(generation, cfg_rev),
    );
    seen
}

#[test]
fn filter_typed_with_trace_isolates_rollback_per_plugin() {
    let pipeline = HookPipeline::new();
    let seen_a = register_recording_filter(
        &pipeline,
        "filter.after",
        10,
        ("plugin.a", "comp.a", 4, 2),
        vec![
            Patch::Drop("c3".into()),
            Patch::Rescore(RescorePatch {
                handle: "c1".into(),
                delta: 7,
                reason: "boost good release".into(),
            }),
        ],
    );
    let seen_b = register_recording_filter(
        &pipeline,
        "filter.after",
        20,
        ("plugin.b", "comp.b", 9, 5),
        vec![
            Patch::Drop("c2".into()),
            Patch::SetOrder(vec!["c1".into(), "c2".into(), "c_rejected".into()]),
        ],
    );

    let mut candidates = vec![
        candidate("c1", 0),
        candidate("c2", 50),
        candidate("c3", 100),
    ];
    let mut admitted = vec!["c1".into(), "c2".into(), "c3".into()];
    let mut budget = RescoreBudget::new(&[0, 100]);
    let fa_payload = FilterPayload::FilterAfter(FilterAfter {
        candidates: candidates.clone(),
        admitted_handles: admitted.clone(),
    });
    let trace = pipeline
        .filter_candidates_with_trace(
            "filter.after",
            "wf-1",
            &base_context(),
            &mut candidates,
            Some(&mut admitted),
            Some(&mut budget),
            &fa_payload,
        )
        .unwrap();

    let surviving: Vec<&str> = candidates.iter().map(|c| c.handle.as_str()).collect();
    assert_eq!(surviving, vec!["c1", "c2"]);
    assert_eq!(admitted, vec!["c1", "c2"]);
    assert_eq!(candidates[0].score, Some(7));

    let ctx_a = seen_a.lock().unwrap()[0].clone();
    let ctx_b = seen_b.lock().unwrap()[0].clone();
    assert_eq!(
        (
            ctx_a.plugin_id.as_str(),
            ctx_a.generation,
            ctx_a.config_revision
        ),
        ("plugin.a", 4, 2)
    );
    assert_eq!(
        (
            ctx_b.plugin_id.as_str(),
            ctx_b.generation,
            ctx_b.config_revision
        ),
        ("plugin.b", 9, 5)
    );
    assert_ne!(ctx_a.invocation_id, ctx_b.invocation_id);

    assert_eq!(trace.steps.len(), 2);
    assert_eq!(trace.steps[0].status, HookStepStatus::Applied);
    assert_eq!(trace.steps[1].status, HookStepStatus::RolledBack);
}

#[test]
fn cumulative_rescore_budget_rolls_back_subscribe_score_after_filter_after() {
    let mut candidates = vec![candidate("c1", 0), candidate("c2", 100)];
    let mut admitted = vec!["c1".into(), "c2".into()];
    let mut budget = RescoreBudget::new(&[0, 100]);

    let fa_pipeline = HookPipeline::new();
    register_recording_filter(
        &fa_pipeline,
        "filter.after",
        10,
        ("plugin.a", "comp.a", 4, 2),
        vec![Patch::Rescore(RescorePatch {
            handle: "c1".into(),
            delta: 7,
            reason: "boost".into(),
        })],
    );
    let fa_payload = FilterPayload::FilterAfter(FilterAfter {
        candidates: candidates.clone(),
        admitted_handles: admitted.clone(),
    });
    fa_pipeline
        .filter_candidates_with_trace(
            "filter.after",
            "wf-1",
            &base_context(),
            &mut candidates,
            Some(&mut admitted),
            Some(&mut budget),
            &fa_payload,
        )
        .unwrap();

    let score_pipeline = HookPipeline::new();
    register_recording_filter(
        &score_pipeline,
        "subscribe.score",
        10,
        ("plugin.a", "comp.a", 4, 2),
        vec![Patch::Rescore(RescorePatch {
            handle: "c1".into(),
            delta: 5,
            reason: "exceeds cumulative cap".into(),
        })],
    );
    let score_payload = FilterPayload::SubscribeScore(SubscribeScore {
        subscribe_id: "sub-1".into(),
        media_id: "m-1".into(),
        candidates: candidates.clone(),
    });
    let score_trace = score_pipeline
        .filter_candidates_with_trace(
            "subscribe.score",
            "wf-1",
            &base_context(),
            &mut candidates,
            None,
            Some(&mut budget),
            &score_payload,
        )
        .unwrap();
    assert_eq!(candidates[0].score, Some(7));
    assert_eq!(score_trace.steps[0].status, HookStepStatus::RolledBack);
}

#[test]
fn download_before_patch_tag_namespace_and_closed_gate_defer_attribution() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let pipeline = HookPipeline::with_failure_policy(GateFailurePolicy::Closed);
    pipeline.register(
        Registration::for_hook(
            "download.before",
            10,
            "plugin.dl",
            "comp.dl",
            Arc::new(RecordingFilter {
                patches: vec![Patch::DownloadPlan(DownloadPlanPatch {
                    add_tags: vec!["forged-unprefixed-tag".into()],
                    paused: Some(true),
                    ..Default::default()
                })],
                seen_contexts: Arc::clone(&seen),
            }),
        )
        .with_metadata(2, 1),
    );
    pipeline.register(
        Registration::for_hook(
            "download.before",
            20,
            "plugin.gate",
            "comp.gate",
            Arc::new(FailingGate {
                result: Err(
                    "wasm trap at 0x1234 https://pt.example/d.php?passkey=SECRET123".into(),
                ),
                seen_contexts: Arc::clone(&seen),
            }),
        )
        .with_metadata(3, 1),
    );

    let mut plan = DownloadPlanView {
        downloader_id: "dl-1".into(),
        relative_directory: None,
        category: None,
        tags: vec![],
        paused: false,
        upload_limit: None,
        download_limit: None,
        sequential: false,
        first_last_piece: false,
    };
    let dl_payload = FilterPayload::DownloadBefore(Box::new(DownloadBefore {
        candidate: candidate("c1", 50),
        purpose: "subscribe".into(),
        plan: plan.clone(),
    }));
    let patch_trace = pipeline
        .filter_download_plan_with_trace(
            "wf-dl",
            &base_context(),
            &mut plan,
            &["dl-1".into()],
            &dl_payload,
        )
        .unwrap();
    assert!(!plan.paused);
    assert!(plan.tags.is_empty());
    assert_eq!(patch_trace.steps[0].status, HookStepStatus::RolledBack);

    let (gate_outcome, gate_trace) = pipeline
        .gate_typed_with_trace(
            "wf-dl",
            &GateInput {
                schema_version: 1,
                context: base_context(),
                hook_id: "download.before".into(),
                handles: vec!["c1".into()],
                revision: 1,
                payload: GatePayload::None,
            },
        )
        .unwrap();
    match gate_outcome {
        Gate::Defer { reason, .. } => {
            assert!(!reason.contains("SECRET123"));
            assert!(!reason.contains("0x1234"));
        }
        other => panic!("expected Gate::Defer on closed download.before failure, got {other:?}"),
    }
    assert_eq!(gate_trace.decision, "defer");
    assert_eq!(gate_trace.steps.len(), 2);
    assert_eq!(gate_trace.steps[0].plugin_id, "plugin.dl");
    assert_eq!(gate_trace.steps[0].decision, "abstain");
    assert_eq!(gate_trace.steps[1].plugin_id, "plugin.gate");
    assert_eq!(gate_trace.steps[1].status, HookStepStatus::Error);
    assert!(
        !gate_trace
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("SECRET123")
    );
}

#[test]
fn notify_typed_with_trace_isolates_failures_and_sanitizer_redacts_secrets() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let pipeline = HookPipeline::new();
    pipeline.register(
        Registration::for_hook(
            "site.checkin.after",
            10,
            "plugin.fail",
            "c1",
            Arc::new(RecordingNotify {
                result: Err("wasm backtrace: trap! cookie=session_secret token=tok123".into()),
                calls: Arc::clone(&calls),
                label: "fail".into(),
            }),
        )
        .with_metadata(1, 1),
    );
    pipeline.register(
        Registration::for_hook(
            "site.checkin.after",
            20,
            "plugin.ok",
            "c2",
            Arc::new(RecordingNotify {
                result: Ok(()),
                calls: Arc::clone(&calls),
                label: "ok".into(),
            }),
        )
        .with_metadata(2, 1),
    );

    let trace = pipeline.notify_typed_with_trace(
        "wf-checkin",
        &Event {
            schema_version: 1,
            context: base_context(),
            id: "ev-1".into(),
            kind: "site.checkin.after".into(),
            occurred_at: 100,
            handles: vec!["site-1".into()],
            revision: 1,
            payload: EventPayload::None,
        },
    );
    assert_eq!(calls.lock().unwrap().len(), 2);
    assert_eq!(trace.steps.len(), 2);
    assert_eq!(trace.steps[0].status, HookStepStatus::Error);
    assert_eq!(trace.steps[1].status, HookStepStatus::Applied);
    let err_msg = trace.steps[0].reason.as_deref().unwrap_or("");
    assert!(!err_msg.contains("session_secret"));
    assert!(!err_msg.contains("tok123"));

    let sanitized = sanitize_reason(
        "fetch failed https://pt.example/get?passkey=abc123&token=xyz cookie=c123 wasm trap: unreachable",
    );
    assert!(!sanitized.contains("abc123"));
    assert!(!sanitized.contains("xyz"));
    assert!(!sanitized.contains("c123"));
    assert!(!sanitized.contains("wasm trap"));
}

#[test]
fn sanitize_reason_redacts_pass_headers_and_json_secrets() {
    let raw_kv = "url=https://pt.example/d.php?id=9&pass=SECRET_PASS_KV&token=TOK_KV";
    let s1 = sanitize_reason(raw_kv);
    assert!(!s1.contains("SECRET_PASS_KV"), "s1={s1}");
    assert!(!s1.contains("TOK_KV"), "s1={s1}");
    assert!(s1.contains("[REDACTED]"), "s1={s1}");

    let raw_hdr = "Cookie: uid=1; pass=SECRET_COOKIE\nAuthorization: Bearer tok123\nAuthorization: Basic dXNlcjpwYXNz\nX-Api-Key: sk_live_123";
    let s2 = sanitize_reason(raw_hdr);
    for leaked in [
        "uid=1",
        "SECRET_COOKIE",
        "tok123",
        "dXNlcjpwYXNz",
        "sk_live_123",
    ] {
        assert!(!s2.contains(leaked), "leaked {leaked} in s2={s2}");
    }

    let raw_json = r#"{"passkey":"PK_RAW","token":"TOK_RAW","other":"ok"}"#;
    let s3 = sanitize_reason(raw_json);
    assert!(!s3.contains("PK_RAW"), "s3={s3}");
    assert!(!s3.contains("TOK_RAW"), "s3={s3}");
    assert!(s3.contains("ok"), "s3={s3}");

    let debug_escaped = r#"guest API error: ApiError { code: Upstream, message: "Cookie: uid=9; pass=SECRET_ESC {\"passkey\":\"PK_ESC\"}" }"#;
    let s4 = sanitize_reason(debug_escaped);
    assert!(!s4.contains("uid=9"), "s4={s4}");
    assert!(!s4.contains("SECRET_ESC"), "s4={s4}");
    assert!(!s4.contains("PK_ESC"), "s4={s4}");
    assert!(s4.contains("[REDACTED]"), "s4={s4}");
}

#[test]
fn sanitize_reason_redacts_url_userinfo_and_oauth_token_query_params() {
    let s1 = sanitize_reason("https://user:SUPER_SECRET_PASS@pt.example/d.torrent?passkey=PK_1");
    assert!(!s1.contains("SUPER_SECRET_PASS"), "s1={s1}");
    assert!(!s1.contains("PK_1"), "s1={s1}");
    assert!(s1.contains("https://[REDACTED]@pt.example"), "s1={s1}");

    let s2 = sanitize_reason("https://token:TOK_IN_URL@pt.example/d.torrent");
    assert!(!s2.contains("TOK_IN_URL"), "s2={s2}");
    assert!(s2.contains("https://[REDACTED]@pt.example"), "s2={s2}");

    let s3 = sanitize_reason("client_secret=CS_VAL_123&x-auth-token=XAT_123&X_AUTH_TOKEN=XAT_UP");
    assert!(!s3.contains("CS_VAL_123"), "s3={s3}");
    assert!(!s3.contains("XAT_123"), "s3={s3}");
    assert!(!s3.contains("XAT_UP"), "s3={s3}");

    let s4 = sanitize_reason("contact alice@example.com for help");
    assert!(s4.contains("alice@example.com"), "s4={s4}");
}

struct RenameStepStub {
    res: Result<Vec<Patch>, Failure>,
    seen_paths: Arc<Mutex<Vec<String>>>,
}
impl PluginInvoker for RenameStepStub {
    fn filter_typed(&self, input: &FilterInput) -> Result<Vec<Patch>, Failure> {
        if let FilterPayload::TransferRename(tr) = &input.payload {
            self.seen_paths
                .lock()
                .unwrap()
                .push(tr.proposed_relative_path.clone());
        }
        self.res.clone()
    }
}

fn reg_rename(
    pipeline: &HookPipeline,
    prio: i32,
    plugin: &str,
    res: Result<Vec<Patch>, Failure>,
) -> Arc<Mutex<Vec<String>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    pipeline.register(Registration::for_hook(
        "transfer.rename",
        prio,
        plugin,
        "c1",
        Arc::new(RenameStepStub {
            res,
            seen_paths: Arc::clone(&seen),
        }),
    ));
    seen
}

#[test]
fn filter_transfer_rename_with_trace_chains_valid_paths_and_rolls_back_invalid_steps() {
    use plugin_api::{ApiError, ErrorCode, TransferRename};

    let pipeline = HookPipeline::new();
    let seen_p1 = reg_rename(
        &pipeline,
        10,
        "p1",
        Ok(vec![Patch::RenamePath(
            "Movies/Dune (2021)/Dune.mkv".into(),
        )]),
    );
    let seen_p2 = reg_rename(
        &pipeline,
        20,
        "p2",
        Ok(vec![Patch::RenamePath("../escape.mkv".into())]),
    );
    let _seen_p3 = reg_rename(&pipeline, 30, "p3", Ok(vec![Patch::Drop("h1".into())]));
    let _seen_p4 = reg_rename(
        &pipeline,
        40,
        "p4",
        Ok(vec![
            Patch::RenamePath("a.mkv".into()),
            Patch::RenamePath("b.mkv".into()),
        ]),
    );
    let seen_p5 = reg_rename(
        &pipeline,
        50,
        "p5",
        Err(Failure::Guest(ApiError {
            code: ErrorCode::Upstream,
            message: "https://u:SECRET_PASS@pt.example/x?token=SECRET_TOK".into(),
            detail: None,
            retry_after_ms: None,
            operation_id: None,
        })),
    );

    let mut rename = TransferRename {
        media_id: "m1".into(),
        library_id: "l1".into(),
        source_path: "/dl/Dune.mkv".into(),
        proposed_relative_path: "Dune.mkv".into(),
        is_movie: true,
    };
    let trace = pipeline
        .filter_transfer_rename_with_trace("wf-transfer-1", &base_context(), &mut rename)
        .unwrap();

    assert_eq!(rename.proposed_relative_path, "Movies/Dune (2021)/Dune.mkv");
    assert_eq!(seen_p1.lock().unwrap().as_slice(), ["Dune.mkv"]);
    assert_eq!(
        seen_p2.lock().unwrap().as_slice(),
        ["Movies/Dune (2021)/Dune.mkv"]
    );
    assert_eq!(
        seen_p5.lock().unwrap().as_slice(),
        ["Movies/Dune (2021)/Dune.mkv"]
    );
    assert_eq!(trace.hook_id, "transfer.rename");
    assert_eq!(trace.steps.len(), 5);
    assert_eq!(trace.steps[0].status, HookStepStatus::Applied);
    for step in &trace.steps[1..=4] {
        assert_eq!(step.status, HookStepStatus::RolledBack);
    }
    let reason = trace.steps[4].reason.as_deref().unwrap();
    assert!(!reason.contains("SECRET_PASS"));
    assert!(!reason.contains("SECRET_TOK"));
}

#[test]
fn classify_notify_failure_maps_retryable_cancelled_and_permanent() {
    use hooks::{NotifyErrorDisposition, classify_notify_failure};
    use plugin_api::{ApiError, ErrorCode, ErrorDetail, UpstreamKind};

    assert_eq!(
        classify_notify_failure(&Failure::Busy),
        NotifyErrorDisposition::Retryable {
            retry_after_ms: None
        }
    );
    assert_eq!(
        classify_notify_failure(&Failure::DeadlineExceeded),
        NotifyErrorDisposition::Retryable {
            retry_after_ms: None
        }
    );
    assert_eq!(
        classify_notify_failure(&Failure::Guest(ApiError {
            code: ErrorCode::Upstream,
            message: "rate limited".into(),
            detail: Some(ErrorDetail::Upstream(UpstreamKind::RateLimited)),
            retry_after_ms: Some(750),
            operation_id: None,
        })),
        NotifyErrorDisposition::Retryable {
            retry_after_ms: Some(750)
        }
    );
    assert_eq!(
        classify_notify_failure(&Failure::Cancelled),
        NotifyErrorDisposition::Cancelled
    );
    for perm in [
        Failure::InvalidComponent("bad".into()),
        Failure::Trap("wasm trap".into()),
        Failure::Guest(ApiError {
            code: ErrorCode::InvalidArgument,
            message: "bad".into(),
            detail: None,
            retry_after_ms: None,
            operation_id: None,
        }),
        Failure::Guest(ApiError {
            code: ErrorCode::PermissionDenied,
            message: "denied".into(),
            detail: None,
            retry_after_ms: None,
            operation_id: None,
        }),
    ] {
        assert_eq!(
            classify_notify_failure(&perm),
            NotifyErrorDisposition::Permanent
        );
    }
}

#[test]
fn notify_typed_outcomes_preserve_disposition_and_cause_id() {
    use hooks::NotifyErrorDisposition;
    use plugin_api::{ApiError, ErrorCode, ErrorDetail, UpstreamKind};

    struct TypedNotifyStub {
        res: Result<(), Failure>,
        seen_causes: Arc<Mutex<Vec<String>>>,
    }
    impl PluginInvoker for TypedNotifyStub {
        fn notify_typed_result(&self, event: &Event) -> Result<(), Failure> {
            self.seen_causes
                .lock()
                .unwrap()
                .push(event.context.cause_id.clone());
            self.res.clone()
        }
    }

    let seen = Arc::new(Mutex::new(Vec::new()));
    let pipeline = HookPipeline::new();
    pipeline.register(Registration::for_hook(
        "transfer.completed",
        10,
        "p-retry",
        "c1",
        Arc::new(TypedNotifyStub {
            res: Err(Failure::Guest(ApiError {
                code: ErrorCode::Upstream,
                message: "429 token=SECRET_429".into(),
                detail: Some(ErrorDetail::Upstream(UpstreamKind::RateLimited)),
                retry_after_ms: Some(500),
                operation_id: None,
            })),
            seen_causes: Arc::clone(&seen),
        }),
    ));
    pipeline.register(Registration::for_hook(
        "transfer.completed",
        20,
        "p-ok",
        "c2",
        Arc::new(TypedNotifyStub {
            res: Ok(()),
            seen_causes: Arc::clone(&seen),
        }),
    ));

    let event = Event {
        schema_version: 1,
        context: base_context(),
        id: "ev-tc-1".into(),
        kind: "transfer.completed".into(),
        occurred_at: 1_700_000_000_000,
        handles: vec![],
        revision: 1,
        payload: EventPayload::None,
    };
    let (trace, outcomes) = pipeline.notify_typed_outcomes_with_trace("wf-tc-1", &event);
    assert_eq!(seen.lock().unwrap().as_slice(), ["cause-1", "cause-1"]);
    assert_eq!(trace.decision, "notified_with_errors");
    assert_eq!(outcomes.len(), 2);
    assert_eq!(
        outcomes[0].disposition,
        Some(NotifyErrorDisposition::Retryable {
            retry_after_ms: Some(500)
        })
    );
    assert!(
        !outcomes[0]
            .sanitized_reason
            .as_deref()
            .unwrap()
            .contains("SECRET_429")
    );
    assert!(outcomes[1].failure.is_none());
    assert!(outcomes[1].disposition.is_none());
}

#[test]
fn sanitize_reason_redacts_escaped_quotes_and_truncated_json_values() {
    let escaped =
        sanitize_reason(r#"upstream failed: {"password": "my\"secret\"123", "ok": true}"#);
    assert!(!escaped.contains("secret"), "{escaped}");
    assert!(!escaped.contains("123"), "{escaped}");
    assert!(escaped.contains(r#""password": "[REDACTED]""#), "{escaped}");

    let truncated = sanitize_reason(r#"body={"access_token": "tok_unclosed_secret_value"#);
    assert!(
        !truncated.contains("tok_unclosed_secret_value"),
        "{truncated}"
    );
    assert!(truncated.contains("[REDACTED]"), "{truncated}");

    let outer_escaped =
        sanitize_reason(r#"payload="{\"api_key\": \"abc\\\"inner_secret\\\"xyz\"}""#);
    assert!(!outer_escaped.contains("inner_secret"), "{outer_escaped}");
    assert!(!outer_escaped.contains("xyz"), "{outer_escaped}");

    let outer_truncated = sanitize_reason(r#"payload="{\"secret\": \"outer_unclosed_secret"#);
    assert!(
        !outer_truncated.contains("outer_unclosed_secret"),
        "{outer_truncated}"
    );
    assert!(outer_truncated.contains("[REDACTED]"), "{outer_truncated}");
}

#[test]
fn sanitize_reason_and_redact_urls_handle_multibyte_utf8_url_path_segments() {
    let s1 = sanitize_reason("https://pt.example/a中文/abé_test/1.torrent?passkey=SECRET_PK");
    assert!(!s1.contains("SECRET_PK"), "{s1}");
    assert!(s1.contains("a中文"), "{s1}");

    let s2 = sanitize_reason("https://api.telegram.org/bot123456:SECRET_BOT_TOK/sendMessage");
    assert!(!s2.contains("SECRET_BOT_TOK"), "{s2}");
    assert!(s2.contains("bot123456:[REDACTED]"), "{s2}");

    let s3 = hooks::redact_urls("failed at https://pt.example/a中文/1.torrent?passkey=SECRET_PK");
    assert!(!s3.contains("SECRET_PK"), "{s3}");
    assert!(s3.contains("[redacted-url]"), "{s3}");
}
