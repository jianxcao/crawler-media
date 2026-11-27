use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use hooks::{
    Gate, GateFailurePolicy, HookPipeline, PluginInvoker, Registration, RescoreBudget,
    rescore_delta_cap, validate_and_apply_download_plan_patch,
    validate_and_apply_search_query_patch, validate_rename_path, validate_select_handles,
};
use plugin_api::{
    DownloadPlanPatch, DownloadPlanView, Event, EventPayload, Failure, FilterInput, FilterPayload,
    GateInput, GatePayload, HookContext, Patch, SearchBefore, SearchQuery, SearchQueryPatch,
};

fn test_context(plugin_id: &str) -> HookContext {
    HookContext {
        invocation_id: "inv-1".into(),
        cause_id: "cause-1".into(),
        plugin_id: plugin_id.into(),
        generation: 1,
        config_revision: 1,
        subject: "subject:1".into(),
        site_id: None,
        subscribe_id: None,
        library_id: None,
        deadline_ms: 9_999_999,
        cancelled: false,
    }
}

struct TypedStub {
    gate_result: Result<Gate, String>,
    notify_result: Result<(), String>,
    called: Arc<AtomicUsize>,
}

impl PluginInvoker for TypedStub {
    fn filter_typed(&self, _input: &FilterInput) -> Result<Vec<Patch>, Failure> {
        self.called.fetch_add(1, Ordering::SeqCst);
        Ok(vec![Patch::RenamePath("Season 01/Ep01.mkv".into())])
    }

    fn gate_typed(&self, _input: &GateInput) -> Result<Gate, String> {
        self.called.fetch_add(1, Ordering::SeqCst);
        self.gate_result.clone()
    }

    fn notify_typed(&self, _event: &Event) -> Result<(), String> {
        self.called.fetch_add(1, Ordering::SeqCst);
        self.notify_result.clone()
    }
}

#[test]
fn gate_defer_short_circuits_and_closed_policy_propagates_errors() {
    let second_calls = Arc::new(AtomicUsize::new(0));
    let pipeline = HookPipeline::new();
    pipeline.register(Registration::for_hook(
        "download.before",
        10,
        "p1",
        "c1",
        Arc::new(TypedStub {
            gate_result: Ok(Gate::Defer {
                until_ms: 5_000,
                reason: "quota-busy".into(),
            }),
            notify_result: Ok(()),
            called: Arc::new(AtomicUsize::new(0)),
        }),
    ));
    pipeline.register(Registration::for_hook(
        "download.before",
        20,
        "p2",
        "c2",
        Arc::new(TypedStub {
            gate_result: Ok(Gate::Allow),
            notify_result: Ok(()),
            called: Arc::clone(&second_calls),
        }),
    ));

    let gate_input = GateInput {
        schema_version: 1,
        context: test_context("p1"),
        hook_id: "download.before".into(),
        handles: vec!["h1".into()],
        revision: 1,
        payload: GatePayload::None,
    };
    let decision = pipeline.gate_typed(&gate_input).unwrap();
    assert_eq!(
        decision,
        Gate::Defer {
            until_ms: 5_000,
            reason: "quota-busy".into()
        }
    );
    assert_eq!(second_calls.load(Ordering::SeqCst), 0);

    let closed = HookPipeline::with_failure_policy(GateFailurePolicy::Closed);
    closed.register(Registration::for_hook(
        "download.before",
        10,
        "p1",
        "c1",
        Arc::new(TypedStub {
            gate_result: Err("boom".into()),
            notify_result: Ok(()),
            called: Arc::new(AtomicUsize::new(0)),
        }),
    ));
    assert!(closed.gate_typed(&gate_input).is_err());
}

#[test]
fn registration_for_hook_isolates_hook_ids() {
    let other_calls = Arc::new(AtomicUsize::new(0));
    let rename_calls = Arc::new(AtomicUsize::new(0));
    let pipeline = HookPipeline::new();
    pipeline.register(Registration::for_hook(
        "search.after",
        10,
        "p1",
        "c1",
        Arc::new(TypedStub {
            gate_result: Ok(Gate::Allow),
            notify_result: Ok(()),
            called: Arc::clone(&other_calls),
        }),
    ));
    pipeline.register(Registration::for_hook(
        "transfer.rename",
        10,
        "p2",
        "c2",
        Arc::new(TypedStub {
            gate_result: Ok(Gate::Allow),
            notify_result: Ok(()),
            called: Arc::clone(&rename_calls),
        }),
    ));

    let input = FilterInput {
        schema_version: 1,
        context: test_context("p2"),
        payload: FilterPayload::TransferRename(plugin_api::TransferRename {
            media_id: "m1".into(),
            library_id: "l1".into(),
            source_path: "/dl/a.mkv".into(),
            proposed_relative_path: "Movie/a.mkv".into(),
            is_movie: true,
        }),
    };
    let (patches, diags) = pipeline.filter_typed("transfer.rename", &input).unwrap();
    assert!(diags.is_empty());
    assert_eq!(patches.len(), 1);
    assert_eq!(other_calls.load(Ordering::SeqCst), 0);
    assert_eq!(rename_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn rescore_budget_caps_cumulative_delta_across_filter_after_and_subscribe_score() {
    assert_eq!(rescore_delta_cap(&[100, 120]), 10);
    assert_eq!(rescore_delta_cap(&[0, 500]), 50);

    let mut budget = RescoreBudget::new(&[0, 200]); // cap = 20
    assert_eq!(budget.try_apply("p1", "cand-1", 12).unwrap(), 12);
    assert_eq!(budget.try_apply("p1", "cand-1", 8).unwrap(), 20);
    assert!(matches!(
        budget.try_apply("p1", "cand-1", 1),
        Err(Failure::InvalidPatch(_))
    ));
    // Independent plugin/candidate gets its own budget.
    assert_eq!(budget.try_apply("p2", "cand-1", -20).unwrap(), -20);
}

#[test]
fn download_plan_patch_enforces_downloader_allowlist_normal_path_and_tag_namespace() {
    let mut plan = DownloadPlanView {
        downloader_id: "dl-1".into(),
        relative_directory: None,
        category: None,
        tags: vec!["user-tag".into()],
        paused: false,
        upload_limit: None,
        download_limit: None,
        sequential: false,
        first_last_piece: false,
    };
    let allowed = vec!["dl-1".into(), "dl-2".into()];

    let bad_dl = DownloadPlanPatch {
        downloader_id: Some("dl-unknown".into()),
        ..Default::default()
    };
    assert!(validate_and_apply_download_plan_patch(&mut plan, &bad_dl, &allowed, "p1").is_err());

    let bad_path = DownloadPlanPatch {
        relative_directory: Some("../escape".into()),
        ..Default::default()
    };
    assert!(validate_and_apply_download_plan_patch(&mut plan, &bad_path, &allowed, "p1").is_err());

    let bad_tag = DownloadPlanPatch {
        add_tags: vec!["crawler-media-owned-fake".into()],
        ..Default::default()
    };
    assert!(validate_and_apply_download_plan_patch(&mut plan, &bad_tag, &allowed, "p1").is_err());

    let good = DownloadPlanPatch {
        downloader_id: Some("dl-2".into()),
        relative_directory: Some("movies/4k".into()),
        add_tags: vec!["plugin:p1:boost".into()],
        paused: Some(true),
        ..Default::default()
    };
    validate_and_apply_download_plan_patch(&mut plan, &good, &allowed, "p1").unwrap();
    assert_eq!(plan.downloader_id, "dl-2");
    assert_eq!(plan.relative_directory.as_deref(), Some("movies/4k"));
    assert!(plan.tags.contains(&"plugin:p1:boost".into()));
    assert!(plan.paused);
}

#[test]
fn search_query_patch_enforces_site_subset_and_select_handles_prevents_revival() {
    let mut query = SearchQuery {
        keyword: "Dune".into(),
        page: 1,
        categories: vec![],
    };
    let mut sites = vec!["s1".into(), "s2".into()];
    let bad = SearchQueryPatch {
        site_ids: Some(vec!["s1".into(), "s3".into()]),
        ..Default::default()
    };
    assert!(validate_and_apply_search_query_patch(&mut query, &mut sites, &bad).is_err());

    let good = SearchQueryPatch {
        keyword: Some("Dune Part Two".into()),
        site_ids: Some(vec!["s2".into()]),
        categories: Some(vec!["movie".into()]),
    };
    validate_and_apply_search_query_patch(&mut query, &mut sites, &good).unwrap();
    assert_eq!(sites, vec!["s2"]);
    assert_eq!(query.keyword, "Dune Part Two");

    let admitted = vec!["h1".into(), "h2".into()];
    assert!(validate_select_handles(&admitted, &["h1".into(), "forged".into()]).is_err());
    assert!(validate_select_handles(&admitted, &["h1".into(), "h1".into()]).is_err());
    assert_eq!(
        validate_select_handles(&admitted, &["h2".into()]).unwrap(),
        vec!["h2"]
    );
}

#[test]
fn transfer_rename_rejects_traversal_and_notify_isolates_failures() {
    assert!(validate_rename_path("/abs/path.mkv").is_err());
    assert!(validate_rename_path("../escape.mkv").is_err());
    assert!(validate_rename_path("bad\\slash.mkv").is_err());
    assert_eq!(
        validate_rename_path("Show/Season 01/E01.mkv").unwrap(),
        "Show/Season 01/E01.mkv"
    );

    let ok_calls = Arc::new(AtomicUsize::new(0));
    let pipeline = HookPipeline::new();
    pipeline.register(Registration::for_hook(
        "download.after",
        10,
        "failing",
        "c1",
        Arc::new(TypedStub {
            gate_result: Ok(Gate::Allow),
            notify_result: Err("notify err".into()),
            called: Arc::new(AtomicUsize::new(0)),
        }),
    ));
    pipeline.register(Registration::for_hook(
        "download.after",
        20,
        "healthy",
        "c2",
        Arc::new(TypedStub {
            gate_result: Ok(Gate::Allow),
            notify_result: Ok(()),
            called: Arc::clone(&ok_calls),
        }),
    ));

    let event = Event {
        schema_version: 1,
        context: test_context("host"),
        id: "ev-1".into(),
        kind: "download.after".into(),
        occurred_at: 123,
        handles: vec!["h1".into()],
        revision: 1,
        payload: EventPayload::None,
    };
    let diags = pipeline.notify_typed(&event);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].0, "failing");
    assert_eq!(ok_calls.load(Ordering::SeqCst), 1);

    let _unused = SearchBefore {
        query: SearchQuery {
            keyword: "k".into(),
            page: 1,
            categories: vec![],
        },
        site_ids: vec![],
    };
}

#[test]
fn search_query_patch_and_download_plan_patch_are_atomic_on_error() {
    let mut query = SearchQuery {
        keyword: "Dune".into(),
        page: 1,
        categories: vec!["movie".into()],
    };
    let mut sites = vec!["s1".into(), "s2".into()];
    let bad_patch = SearchQueryPatch {
        site_ids: Some(vec!["s2".into()]),
        keyword: Some("   ".into()),
        categories: Some(vec!["tv".into()]),
    };
    let res = validate_and_apply_search_query_patch(&mut query, &mut sites, &bad_patch);
    assert!(matches!(res, Err(Failure::InvalidPatch(_))));
    assert_eq!(sites, vec!["s1", "s2"]);
    assert_eq!(query.keyword, "Dune");
    assert_eq!(query.categories, vec!["movie"]);

    let mut plan = DownloadPlanView {
        downloader_id: "dl-1".into(),
        relative_directory: None,
        category: None,
        tags: vec!["user-tag".into()],
        paused: false,
        upload_limit: None,
        download_limit: None,
        sequential: false,
        first_last_piece: false,
    };
    let allowed = vec!["dl-1".into(), "dl-2".into()];
    let bad_plan_patch = DownloadPlanPatch {
        downloader_id: Some("dl-2".into()),
        relative_directory: Some("movies/4k".into()),
        add_tags: vec!["user:forged".into()],
        paused: Some(true),
        ..Default::default()
    };
    let plan_res =
        validate_and_apply_download_plan_patch(&mut plan, &bad_plan_patch, &allowed, "p1");
    assert!(matches!(plan_res, Err(Failure::InvalidPatch(_))));
    assert_eq!(plan.downloader_id, "dl-1");
    assert_eq!(plan.relative_directory, None);
    assert_eq!(plan.tags, vec!["user-tag"]);
    assert!(!plan.paused);
}

#[test]
fn validate_rename_path_rejects_trailing_slash_and_control_chars() {
    assert!(matches!(
        validate_rename_path("Movie/"),
        Err(Failure::InvalidPatch(_))
    ));
    assert!(matches!(
        validate_rename_path("Movie/a\nb.mkv"),
        Err(Failure::InvalidPatch(_))
    ));
    assert!(matches!(
        validate_rename_path("Movie/a\rb.mkv"),
        Err(Failure::InvalidPatch(_))
    ));
    assert!(matches!(
        validate_rename_path("Movie/a\tb.mkv"),
        Err(Failure::InvalidPatch(_))
    ));
}

#[test]
fn download_plan_patch_rejects_comma_control_chars_untrimmed_tags_and_control_category() {
    let base = DownloadPlanView {
        downloader_id: "dl-1".into(),
        relative_directory: None,
        category: Some("orig".into()),
        tags: vec!["plugin:p1:keep".into()],
        paused: false,
        upload_limit: None,
        download_limit: None,
        sequential: false,
        first_last_piece: false,
    };
    let allowed = vec!["dl-1".into(), "dl-2".into()];

    for bad_tag in [
        "plugin:p1:ok,cm-claimed",
        "plugin:p1:ok,cm-owned:abc",
        "plugin:p1:ok,cm-op:1",
        "plugin:p1:ok,cm-sub:1",
        " plugin:p1:ok",
        "plugin:p1:ok ",
        "plugin:p1:ok\ninjected",
        "plugin:p1:ok\t",
    ] {
        let mut plan = base.clone();
        let add_patch = DownloadPlanPatch {
            downloader_id: Some("dl-2".into()),
            add_tags: vec![bad_tag.into()],
            ..Default::default()
        };
        assert!(
            matches!(
                validate_and_apply_download_plan_patch(&mut plan, &add_patch, &allowed, "p1"),
                Err(Failure::InvalidPatch(_))
            ),
            "add_tags must reject `{bad_tag:?}`"
        );
        assert_eq!(plan, base);

        let rem_patch = DownloadPlanPatch {
            downloader_id: Some("dl-2".into()),
            remove_tags: vec![bad_tag.into()],
            ..Default::default()
        };
        assert!(
            matches!(
                validate_and_apply_download_plan_patch(&mut plan, &rem_patch, &allowed, "p1"),
                Err(Failure::InvalidPatch(_))
            ),
            "remove_tags must reject `{bad_tag:?}`"
        );
        assert_eq!(plan, base);
    }

    for bad_cat in [
        "movies\ninjected",
        "movies\r",
        "movies\0",
        "../outside",
        "movies/../outside",
        "/abs/path",
        "movies\\sub",
    ] {
        let mut plan = base.clone();
        let cat_patch = DownloadPlanPatch {
            downloader_id: Some("dl-2".into()),
            category: Some(bad_cat.into()),
            ..Default::default()
        };
        assert!(
            matches!(
                validate_and_apply_download_plan_patch(&mut plan, &cat_patch, &allowed, "p1"),
                Err(Failure::InvalidPatch(_))
            ),
            "category must reject `{bad_cat:?}`"
        );
        assert_eq!(plan, base);
    }
}

#[test]
fn sanitize_reason_redacts_pt_path_passkeys_percent_encoded_secrets_and_member_urls() {
    let raw = "fetch failed: https://secret-pt.internal/torrent/download/77.PATH_PASSKEY?passkey%3DENC_SECRET&sign=RAW_SIGN and https://pt2.internal/dl/88.UNIT3D_KEY and https://pt3.internal/rss/RSS_TOKEN_ABCDEF123456";
    let sanitized = hooks::sanitize_reason(raw);
    for secret in [
        "PATH_PASSKEY",
        "ENC_SECRET",
        "RAW_SIGN",
        "UNIT3D_KEY",
        "RSS_TOKEN_ABCDEF123456",
    ] {
        assert!(
            !sanitized.contains(secret),
            "sanitize_reason leaked `{secret}`: {sanitized}"
        );
    }
    let normal_torrent = hooks::sanitize_reason("ok https://pt.example/download/77.torrent");
    assert!(
        normal_torrent.contains("/download/77.torrent"),
        "normal .torrent extension must remain intact: {normal_torrent}"
    );
}

#[test]
fn sanitize_reason_and_notify_typed_redact_rss_download_passkey_and_webhook_path_tokens() {
    let raw = concat!(
        "pt https://avistaz.to/rss/download/0123456789abcdef0123456789abcdef/77.torrent ",
        "discord https://discord.com/api/webhooks/1234567890/DISCORD_SECRET_TOKEN_XYZ ",
        "slack https://hooks.slack.com/services/T000AAA11/B000BBB22/SLACK_SECRET_TOKEN_XYZ ",
        "tg https://api.telegram.org/bot123456789:TG_SECRET_BOT_TOKEN/sendMessage ",
        "generic https://notify.example/webhooks/GENERIC_HOOK_SECRET_999"
    );
    let sanitized = hooks::sanitize_reason(raw);
    for secret in [
        "0123456789abcdef0123456789abcdef",
        "DISCORD_SECRET_TOKEN_XYZ",
        "SLACK_SECRET_TOKEN_XYZ",
        "TG_SECRET_BOT_TOKEN",
        "GENERIC_HOOK_SECRET_999",
    ] {
        assert!(
            !sanitized.contains(secret),
            "sanitize_reason leaked `{secret}`: {sanitized}"
        );
    }

    struct LeakingNotify;
    impl hooks::PluginInvoker for LeakingNotify {
        fn notify_typed(&self, _event: &plugin_api::Event) -> Result<(), String> {
            Err(
                "POST https://discord.com/api/webhooks/123/NOTIFY_LEAK_TOKEN_ABC failed: 500"
                    .into(),
            )
        }
    }
    let pipeline = hooks::HookPipeline::new();
    pipeline.register(hooks::Registration::for_hook(
        "download.completed",
        0,
        "notify-p",
        "notify-c",
        std::sync::Arc::new(LeakingNotify),
    ));
    let errs = pipeline.notify_typed(&plugin_api::Event {
        schema_version: 1,
        context: plugin_api::HookContext {
            invocation_id: "inv-1".into(),
            cause_id: "c-1".into(),
            plugin_id: String::new(),
            generation: 1,
            config_revision: 1,
            subject: "system".into(),
            site_id: None,
            subscribe_id: None,
            library_id: None,
            deadline_ms: 1000,
            cancelled: false,
        },
        id: "ev-1".into(),
        kind: "download.completed".into(),
        occurred_at: 1,
        handles: Vec::new(),
        revision: 1,
        payload: plugin_api::EventPayload::None,
    });
    assert_eq!(errs.len(), 1);
    assert!(
        !errs[0].2.contains("NOTIFY_LEAK_TOKEN_ABC"),
        "HookPipeline::notify_typed must sanitize returned error: {:?}",
        errs
    );
}
