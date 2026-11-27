use std::sync::Arc;

use domain::SiteId;
use hooks::{
    Gate, GateContext, HookPipeline, PluginInvoker, Registration, SiteAction, SiteTransport,
};

struct Reject;

impl PluginInvoker for Reject {
    fn gate(&self, _: &GateContext) -> Result<Gate, String> {
        Ok(Gate::Reject("denied".into()))
    }
}

#[test]
fn snapshot_releases_registry_and_rejects_recursive_dispatch() {
    let pipeline = Arc::new(HookPipeline::new());
    let weak = Arc::downgrade(&pipeline);
    pipeline.register(Registration::new(
        0,
        "a",
        "one",
        Arc::new(move |context: &GateContext| {
            let pipeline = weak.upgrade().unwrap();
            pipeline.register(Registration::new(1, "b", "two", Arc::new(Reject)));
            assert!(pipeline.gate(context).is_err());
            Ok(Gate::Allow)
        }),
    ));
    let context = GateContext::Site {
        site_id: SiteId::new(),
        action: SiteAction::CheckIn,
        transport: SiteTransport::Http,
    };
    assert_eq!(pipeline.gate(&context), Ok(Gate::Allow));
    assert_eq!(pipeline.gate(&context), Ok(Gate::Reject("denied".into())));
}

#[test]
fn selection_never_revives_removed_handles_or_accepts_duplicates() {
    struct Select(Vec<String>);
    impl PluginInvoker for Select {
        fn select(&self, _: &hooks::SelectionContext) -> Result<Vec<String>, String> {
            Ok(self.0.clone())
        }
    }
    let pipeline = HookPipeline::new();
    pipeline.register(Registration::new(
        1,
        "a",
        "remove",
        Arc::new(Select(vec!["b".into()])),
    ));
    pipeline.register(Registration::new(
        2,
        "b",
        "revive",
        Arc::new(Select(vec!["a".into()])),
    ));
    pipeline.register(Registration::new(
        3,
        "c",
        "duplicate",
        Arc::new(Select(vec!["b".into(), "b".into()])),
    ));
    assert_eq!(pipeline.select(selection(&["a", "b"])).unwrap(), vec!["b"]);
}

struct SelectFirst;
impl PluginInvoker for SelectFirst {
    fn select(&self, context: &hooks::SelectionContext) -> Result<Vec<String>, String> {
        Ok(context
            .candidates
            .first()
            .map(|c| c.handle.clone())
            .into_iter()
            .collect())
    }
}
struct Reverse;
impl PluginInvoker for Reverse {
    fn select(&self, context: &hooks::SelectionContext) -> Result<Vec<String>, String> {
        Ok(context
            .candidates
            .iter()
            .rev()
            .map(|c| c.handle.clone())
            .collect())
    }
}

#[test]
fn selection_runs_in_component_order() {
    let pipeline = HookPipeline::new();
    pipeline.register(Registration::new(0, "a", "z", Arc::new(SelectFirst)));
    pipeline.register(Registration::new(0, "a", "a", Arc::new(Reverse)));
    assert_eq!(pipeline.select(selection(&["a", "b"])).unwrap(), vec!["b"]);
}

#[test]
fn selection_runs_in_priority_order_before_plugin_order() {
    let pipeline = HookPipeline::new();
    pipeline.register(Registration::new(10, "a", "a", Arc::new(SelectFirst)));
    pipeline.register(Registration::new(-1, "z", "z", Arc::new(Reverse)));
    assert_eq!(pipeline.select(selection(&["a", "b"])).unwrap(), vec!["b"]);
}

#[test]
fn selection_runs_in_plugin_order_before_component_order() {
    let pipeline = HookPipeline::new();
    pipeline.register(Registration::new(0, "z", "a", Arc::new(SelectFirst)));
    pipeline.register(Registration::new(0, "a", "z", Arc::new(Reverse)));
    assert_eq!(pipeline.select(selection(&["a", "b"])).unwrap(), vec!["b"]);
}

fn selection(handles: &[&str]) -> hooks::SelectionContext {
    hooks::SelectionContext {
        subscribe_id: domain::SubscribeId::new(),
        media_id: domain::MediaId::new(),
        candidates: handles
            .iter()
            .map(|handle| hooks::SelectionCandidate {
                handle: (*handle).into(),
                site_id: SiteId::new(),
                title: "release".into(),
                score: 1,
            })
            .collect(),
    }
}

#[test]
fn empty_pipeline_allows_and_explicit_reject_is_distinct() {
    let pipeline = HookPipeline::new();
    let context = GateContext::Site {
        site_id: SiteId::new(),
        action: SiteAction::Authenticate,
        transport: SiteTransport::Http,
    };
    assert_eq!(pipeline.gate(&context), Ok(Gate::Allow));
    pipeline.register(Registration::new(
        0,
        "plugin",
        "component",
        Arc::new(Reject),
    ));
    assert_eq!(pipeline.gate(&context), Ok(Gate::Reject("denied".into())));
}

#[test]
fn gate_runtime_failure_is_distinct_from_veto() {
    let pipeline = HookPipeline::with_failure_policy(hooks::GateFailurePolicy::Closed);
    pipeline.register(Registration::new(
        0,
        "p",
        "c",
        Arc::new(|_: &GateContext| Err("runtime failed".into())),
    ));
    let context = GateContext::Site {
        site_id: SiteId::new(),
        action: SiteAction::Authenticate,
        transport: SiteTransport::Http,
    };
    assert_eq!(pipeline.gate(&context), Err("runtime failed".into()));
}

#[test]
fn default_gate_failure_policy_preserves_host_behavior() {
    let pipeline = HookPipeline::new();
    pipeline.register(Registration::new(
        0,
        "p",
        "c",
        Arc::new(|_: &GateContext| Err("runtime failed".into())),
    ));
    let context = GateContext::Site {
        site_id: SiteId::new(),
        action: SiteAction::Authenticate,
        transport: SiteTransport::Http,
    };
    assert_eq!(pipeline.gate(&context), Ok(Gate::Allow));
}

#[test]
fn selection_receives_subscribe_and_candidate_facts() {
    struct Inspect;
    impl PluginInvoker for Inspect {
        fn select(&self, context: &hooks::SelectionContext) -> Result<Vec<String>, String> {
            assert_eq!(context.candidates[0].title, "release");
            assert_eq!(context.candidates[0].score, 42);
            Ok(vec![context.candidates[0].handle.clone()])
        }
    }
    let pipeline = HookPipeline::new();
    pipeline.register(Registration::new(0, "p", "c", Arc::new(Inspect)));
    let candidate = hooks::SelectionCandidate {
        handle: "h".into(),
        site_id: SiteId::new(),
        title: "release".into(),
        score: 42,
    };
    let context = hooks::SelectionContext {
        subscribe_id: domain::SubscribeId::new(),
        media_id: domain::MediaId::new(),
        candidates: vec![candidate],
    };
    assert_eq!(pipeline.select(context).unwrap(), vec!["h"]);
}

#[test]
fn gate_and_gate_typed_sanitize_reject_and_defer_reasons() {
    struct LeakyGate(bool);
    impl PluginInvoker for LeakyGate {
        fn gate(&self, _: &GateContext) -> Result<Gate, String> {
            if self.0 {
                Ok(Gate::Reject(
                    "blocked https://pt.example/rss?passkey=deadbeefcafebabe1234567890abcdef"
                        .into(),
                ))
            } else {
                Ok(Gate::Defer {
                    until_ms: 500,
                    reason: "wait Cookie: uid=1; pass=supersecretcookie999".into(),
                })
            }
        }
        fn gate_typed(&self, _: &plugin_api::GateInput) -> Result<Gate, String> {
            self.gate(&GateContext::Site {
                site_id: SiteId::new(),
                action: SiteAction::CheckIn,
                transport: SiteTransport::Http,
            })
        }
    }

    let ctx = GateContext::Site {
        site_id: SiteId::new(),
        action: SiteAction::CheckIn,
        transport: SiteTransport::Http,
    };
    let typed = plugin_api::GateInput {
        hook_id: "download.before".into(),
        schema_version: 1,
        context: plugin_api::HookContext {
            invocation_id: "inv-1".into(),
            cause_id: "c-1".into(),
            plugin_id: "host".into(),
            generation: 0,
            config_revision: 0,
            subject: "s".into(),
            site_id: None,
            subscribe_id: None,
            library_id: None,
            deadline_ms: 10_000,
            cancelled: false,
        },
        handles: vec![],
        revision: 1,
        payload: plugin_api::GatePayload::None,
    };

    let p_reject = HookPipeline::new();
    p_reject.register(Registration::new(0, "p", "c", Arc::new(LeakyGate(true))));
    let Gate::Reject(r1) = p_reject.gate(&ctx).unwrap() else {
        panic!("expected Reject");
    };
    assert!(!r1.contains("deadbeefcafebabe1234567890abcdef"), "{r1}");
    let Gate::Reject(r2) = p_reject.gate_typed(&typed).unwrap() else {
        panic!("expected typed Reject");
    };
    assert!(!r2.contains("deadbeefcafebabe1234567890abcdef"), "{r2}");

    let p_defer = HookPipeline::new();
    p_defer.register(Registration::new(0, "p", "c", Arc::new(LeakyGate(false))));
    let Gate::Defer { reason: d1, .. } = p_defer.gate(&ctx).unwrap() else {
        panic!("expected Defer");
    };
    assert!(!d1.contains("supersecretcookie999"), "{d1}");
    let Gate::Defer { reason: d2, .. } = p_defer.gate_typed(&typed).unwrap() else {
        panic!("expected typed Defer");
    };
    assert!(!d2.contains("supersecretcookie999"), "{d2}");
}
