use plugin_api::Candidate;
use plugin_host::PluginHost;

fn candidates() -> Vec<Candidate> {
    ["A", "B"]
        .into_iter()
        .map(|name| Candidate {
            handle: format!("opaque-{name}"),
            site_id: "site-1".into(),
            provider: "builtin".into(),
            generation: 7,
            title: name.into(),
            size: Some(100),
            seeders: Some(2),
            leechers: None,
            published_at: None,
            free: None,
            hr: None,
            release: None,
            score: None,
            labels: Vec::new(),
            revision: 7,
        })
        .collect()
}

#[test]
fn real_component_drops_a_retaining_b_host_identity() {
    let mut input = candidates();
    input[1].size = None;
    let outcome = host().filter(component(), batch(input.clone()));
    assert!(outcome.failure.is_none(), "{:?}", outcome.failure);
    assert_eq!(outcome.candidates, vec![input[1].clone()]);
}

#[test]
fn real_component_runs_through_typed_pipeline() {
    use hooks::{HookPipeline, Registration};
    use plugin_host::PolicyInvoker;
    use std::sync::Arc;
    let host = host();
    let policy = host.load(component(), plugin_host::HOOK_WORLD).unwrap();
    let invoker = PolicyInvoker::new(host, policy, || {
        plugin_host::Invocation::new("test", std::time::Duration::from_secs(2))
    });
    let pipeline = HookPipeline::new();
    pipeline.register(Registration::new(0, "test", "policy", Arc::new(invoker)));
    let input = candidates();
    let result = pipeline.search_after(batch(input.clone())).unwrap();
    assert_eq!(result.candidates, vec![input[1].clone()]);
}

#[test]
fn guest_import_rejects_pipeline_and_direct_runtime_reentry() {
    use hooks::{HookPipeline, Registration};
    use plugin_host::{CompiledPolicy, HostPorts, Invocation, PolicyInvoker};
    use std::sync::{Arc, Mutex, OnceLock, Weak, mpsc};
    use std::time::{Duration, Instant};

    struct ReentryPort {
        pipeline: OnceLock<Weak<HookPipeline>>,
        direct: Mutex<Option<(PluginHost, CompiledPolicy)>>,
        results: mpsc::Sender<(
            bool,
            Option<plugin_api::Failure>,
            Option<plugin_api::Failure>,
            Duration,
        )>,
    }
    impl HostPorts for ReentryPort {
        fn authorized(&self, _: &Invocation, _: &str) -> bool {
            true
        }
        fn kv_get(
            &self,
            _: &Invocation,
            _: &str,
        ) -> Result<Option<plugin_host::KvValue>, plugin_api::ApiError> {
            let started = Instant::now();
            let pipeline = self.pipeline.get().unwrap().upgrade().unwrap();
            let rejected = pipeline.search_after(batch(candidates())).is_err();
            let (host, policy) = self.direct.lock().unwrap().as_ref().unwrap().clone();
            let outcome = host.invoke(
                &policy,
                Invocation::new("nested", Duration::from_millis(100)),
                batch(candidates()),
            );
            let filter = host.filter(component(), batch(candidates()));
            self.results
                .send((rejected, outcome.failure, filter.failure, started.elapsed()))
                .unwrap();
            Ok(None)
        }
    }
    let (sender, receiver) = mpsc::channel();
    let port = Arc::new(ReentryPort {
        pipeline: OnceLock::new(),
        direct: Mutex::new(None),
        results: sender,
    });
    let host = host_with_ports(
        plugin_host::Limits {
            workers: 1,
            ..plugin_host::Limits::default()
        },
        port.clone(),
    )
    .unwrap();
    let policy = host.load(component(), plugin_host::HOOK_WORLD).unwrap();
    let pipeline = Arc::new(HookPipeline::new());
    port.pipeline.set(Arc::downgrade(&pipeline)).unwrap();
    *port.direct.lock().unwrap() = Some((host.clone(), policy.clone()));
    pipeline.register(Registration::new(
        0,
        "p",
        "c",
        Arc::new(PolicyInvoker::new(host.clone(), policy.clone(), || {
            Invocation::new("outer", Duration::from_secs(2))
        })),
    ));
    let mut input = candidates();
    input[0].title = "kv".into();
    let output = pipeline.search_after(batch(input.clone())).unwrap();
    assert!(output.diagnostics.is_empty());
    let direct_output = host.invoke(
        &policy,
        Invocation::new("direct-outer", Duration::from_secs(2)),
        batch(input),
    );
    assert!(direct_output.failure.is_none());
    for _ in 0..2 {
        let (pipeline_rejected, runtime_failure, filter_failure, elapsed) =
            receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(runtime_failure, Some(plugin_api::Failure::Reentrant));
        assert_eq!(filter_failure, Some(plugin_api::Failure::Reentrant));
        assert!(
            pipeline_rejected,
            "guest import must not dispatch another Hook"
        );
        assert!(
            elapsed < Duration::from_millis(250),
            "reentry must fail before queue admission: {elapsed:?}"
        );
    }
    *port.direct.lock().unwrap() = None;
    let ordinary = host.invoke(
        &policy,
        Invocation::new("ordinary", Duration::from_secs(2)),
        batch(candidates()),
    );
    assert!(ordinary.failure.is_none());
}

#[test]
fn real_component_filters_native_torrents_without_changing_their_origins() {
    use hooks::{HookPipeline, NativeCandidate, Registration};
    use plugin_host::PolicyInvoker;
    use std::sync::Arc;
    let host = host();
    let policy = host.load(component(), plugin_host::HOOK_WORLD).unwrap();
    let pipeline = HookPipeline::new();
    pipeline.register(Registration::new(
        0,
        "test",
        "policy",
        Arc::new(PolicyInvoker::new(host, policy, || {
            plugin_host::Invocation::new("test", std::time::Duration::from_secs(2))
        })),
    ));
    let site = domain::SiteId::new();
    let torrent = |title: &str, enclosure: &str| domain::Torrent {
        site_id: site,
        title: title.into(),
        enclosure: enclosure.into(),
        size_bytes: None,
        seeders: None,
        free: true,
        hr: None,
        imdb_id: None,
        id: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    };
    let retained = torrent("B", "https://example.test/?passkey=private");
    let result = pipeline
        .filter_native(
            vec![
                NativeCandidate::new("a", "provider", 7, 8, torrent("A", "private-a")),
                NativeCandidate::new("b", "provider", 7, 8, retained.clone()),
            ],
            vec![],
        )
        .unwrap();
    assert_eq!(result.candidates.len(), 1);
    assert_eq!(result.candidates[0].torrent, retained);
    assert_eq!(result.candidates[0].handle, "b");
}

fn component() -> &'static [u8] {
    include_bytes!("fixtures/policy.wasm")
}

#[test]
fn unknown_handle_is_rejected_atomically() {
    let mut input = candidates();
    input[0].title = "unknown".into();
    let outcome = host().filter(component(), batch(input.clone()));
    assert!(matches!(
        outcome.failure,
        Some(plugin_api::Failure::InvalidPatch(_))
    ));
    assert_eq!(outcome.candidates, input);
}

#[test]
fn trap_and_guest_api_error_are_distinct() {
    for (mode, guest) in [("trap", false), ("guest-error", true)] {
        let mut input = candidates();
        input[0].title = mode.into();
        let outcome = host().filter(component(), batch(input.clone()));
        assert!(if guest {
            matches!(outcome.failure, Some(plugin_api::Failure::Guest(_)))
        } else {
            matches!(outcome.failure, Some(plugin_api::Failure::Trap(_)))
        });
        assert_eq!(outcome.candidates, input);
        if let Some(plugin_api::Failure::Guest(error)) = outcome.failure {
            assert_eq!(error.code, plugin_api::ErrorCode::InvalidArgument);
            assert_eq!(
                error.detail,
                Some(plugin_api::ErrorDetail::Upstream(
                    plugin_api::UpstreamKind::Parse
                ))
            );
            assert_eq!(error.retry_after_ms, Some(125));
            assert_eq!(error.operation_id.as_deref(), Some("guest-reference"));
        }
    }
}

#[test]
fn unauthorized_import_returns_permission_denied() {
    let mut input = candidates();
    input[0].title = "kv".into();
    let outcome = host().filter(component(), batch(input.clone()));
    assert!(
        matches!(outcome.failure, Some(plugin_api::Failure::Guest(ref value))
        if value.code == plugin_api::ErrorCode::PermissionDenied)
    );
    assert_eq!(outcome.candidates, input);
}

#[test]
fn each_call_gets_fresh_instance_and_host_context() {
    let host = host();
    for mode in ["state", "state", "context"] {
        let mut input = candidates();
        input[0].title = mode.into();
        let outcome = host.filter(component(), batch(input));
        assert!(outcome.failure.is_none(), "{:?}", outcome.failure);
    }
}

#[test]
fn missing_unknown_and_wrongly_typed_imports_are_rejected() {
    let host = host();
    for component in [
        "(component)",
        "(component (type (instance)) (import \"unknown:package/io@1.0.0\" (instance (type 0))))",
        "(component (import \"crawler:plugin/kv@1.0.0\" (func)))",
        "(component (type $i (instance (export \"not-hosted\" (func)))) (import \"crawler:plugin/kv@1.0.0\" (instance (type $i))))",
        "(component (type (instance)) (import \"wasi:filesystem/types@0.2.0\" (instance (type 0))))",
    ] {
        assert!(matches!(
            host.load(component.as_bytes(), plugin_host::HOOK_WORLD),
            Err(plugin_api::Failure::InvalidComponent(_))
        ));
    }
}

#[test]
fn declared_world_and_exact_package_version_must_match() {
    let host = host();
    for world in [
        "crawler:plugin/hook-plugin@1.0.1",
        "crawler:plugin/task-plugin@1.0.0",
    ] {
        assert!(host.load(component(), world).is_err());
    }
    let mut changed = component().to_vec();
    for index in 0..changed.len() - 6 {
        if &changed[index..index + 6] == b"@1.0.0" {
            changed[index + 5] = b'1';
        }
    }
    assert!(host.load(&changed, plugin_host::HOOK_WORLD).is_err());
    let mut missing = component().to_vec();
    for index in 0..missing.len() - 6 {
        if &missing[index..index + 6] == b"filter" {
            missing[index..index + 6].copy_from_slice(b"fliter");
        }
    }
    assert!(host.load(&missing, plugin_host::HOOK_WORLD).is_err());
    let mut newer_import = component().to_vec();
    let interface = b"crawler:plugin/kv@1.0.0";
    for index in 0..newer_import.len() - interface.len() {
        if &newer_import[index..index + interface.len()] == interface {
            newer_import[index + interface.len() - 1] = b'1';
        }
    }
    assert!(host.load(&newer_import, plugin_host::HOOK_WORLD).is_err());
}

#[test]
fn context_deadline_is_a_fixed_read_only_projection() {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };
    struct Clock(AtomicU64);
    impl plugin_host::HostPorts for Clock {
        fn now_ms(&self) -> u64 {
            self.0.fetch_add(100, Ordering::Relaxed)
        }
    }
    let host = host_with_ports(
        plugin_host::Limits::default(),
        Arc::new(Clock(AtomicU64::new(10_000))),
    )
    .unwrap();
    let mut input = candidates();
    input[0].title = "context".into();
    let outcome = host.filter(component(), batch(input));
    assert!(outcome.failure.is_none(), "{:?}", outcome.failure);
}

#[test]
fn compiled_component_cannot_cross_runtime_engines() {
    let first = host();
    let policy = first.load(component(), plugin_host::HOOK_WORLD).unwrap();
    let second = host();
    let input = candidates();
    let outcome = second.invoke(
        &policy,
        plugin_host::Invocation::new("policy", std::time::Duration::from_millis(100)),
        batch(input.clone()),
    );
    assert!(
        matches!(
            outcome.failure,
            Some(plugin_api::Failure::InvalidComponent(_))
        ),
        "{:?}",
        outcome.failure
    );
    assert_eq!(outcome.candidates, input);
}

#[test]
fn search_after_forwards_known_facts_and_preserves_unknown_facts() {
    let mut candidates = candidates();
    candidates[0].title = "full-view".into();
    candidates[0].leechers = Some(3);
    candidates[0].published_at = Some(1234);
    candidates[0].free = Some(false);
    candidates[0].hr = Some(true);
    candidates[0].release = Some("1080p".into());
    candidates[0].score = Some(-7);
    candidates[0].labels = vec!["known".into()];
    candidates[0].revision = 99;
    candidates[1].size = None;
    candidates[1].seeders = None;
    let input = plugin_api::SearchAfter {
        candidates,
        site_failures: vec![site_failure()],
    };
    let outcome = host().filter(component(), input.clone());
    assert!(outcome.failure.is_none(), "{:?}", outcome.failure);
    assert_eq!(outcome.candidates, vec![input.candidates[1].clone()]);
    assert_eq!(outcome.site_failures, input.site_failures);
}

#[test]
fn site_failures_survive_success_invalid_patch_and_runtime_failure() {
    for mode in ["B", "unknown", "guest-error", "trap"] {
        let mut input = batch(candidates());
        input.candidates[0].title = mode.into();
        input.site_failures = vec![site_failure()];
        let outcome = host().filter(component(), input.clone());
        assert_eq!(outcome.site_failures, input.site_failures, "mode: {mode}");
    }
}

fn site_failure() -> plugin_api::SiteFailure {
    plugin_api::SiteFailure {
        site_id: "failed-site".into(),
        error: plugin_api::ApiError {
            code: plugin_api::ErrorCode::Upstream,
            message: "network failure".into(),
            detail: Some(plugin_api::ErrorDetail::Upstream(
                plugin_api::UpstreamKind::Network,
            )),
            retry_after_ms: None,
            operation_id: None,
        },
    }
}

#[test]
fn duplicate_candidate_handles_are_rejected_before_guest() {
    let mut input = batch(candidates());
    input.candidates[0].title = "trap".into();
    input.candidates[1].handle = input.candidates[0].handle.clone();
    assert_invalid_handles(input);
}

#[test]
fn empty_candidate_handle_is_rejected_before_guest() {
    let mut input = batch(candidates());
    input.candidates[0].title = "trap".into();
    input.candidates[0].handle.clear();
    assert_invalid_handles(input);
}

fn assert_invalid_handles(mut input: plugin_api::SearchAfter) {
    input.site_failures = vec![site_failure()];
    let outcome = host().filter(component(), input.clone());
    assert!(
        matches!(outcome.failure, Some(plugin_api::Failure::InvalidInput(_))),
        "{:?}",
        outcome.failure
    );
    assert_eq!(outcome.candidates, input.candidates);
    assert_eq!(outcome.site_failures, input.site_failures);
}

fn host() -> PluginHost {
    host_with_ports(
        plugin_host::Limits::default(),
        std::sync::Arc::new(plugin_host::NoServices),
    )
    .unwrap()
}
fn host_with_ports(
    limits: plugin_host::Limits,
    ports: std::sync::Arc<dyn plugin_host::HostPorts>,
) -> Result<PluginHost, plugin_api::Failure> {
    Ok(plugin_host::PluginRuntime::new(limits)?.host(ports))
}

fn batch(candidates: Vec<Candidate>) -> plugin_api::SearchAfter {
    plugin_api::SearchAfter {
        candidates,
        site_failures: Vec::new(),
    }
}
