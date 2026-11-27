use plugin_api::{Candidate, Failure};
use plugin_host::{HOOK_WORLD, HostPorts, Invocation, Limits, PluginHost};
use std::{
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

#[test]
fn guest_infinite_loop_is_stopped_by_fuel() {
    let input = vec![candidate("loop")];
    let outcome = host().filter(component(), batch(input.clone()));
    assert!(
        matches!(outcome.failure, Some(Failure::Trap(_))),
        "{:?}",
        outcome.failure
    );
    assert_eq!(outcome.candidates, input);
}

#[test]
fn guest_memory_exhaustion_fails_closed() {
    let outcome = host().filter(component(), batch(vec![candidate("memory")]));
    assert!(
        matches!(outcome.failure, Some(Failure::Trap(_))),
        "{:?}",
        outcome.failure
    );
}

fn component() -> &'static [u8] {
    include_bytes!("fixtures/policy.wasm")
}
fn candidate(title: &str) -> Candidate {
    Candidate {
        handle: "opaque-a".into(),
        site_id: "site".into(),
        provider: "builtin".into(),
        generation: 1,
        title: title.into(),
        size: Some(1),
        seeders: None,
        leechers: None,
        published_at: None,
        free: None,
        hr: None,
        release: None,
        score: None,
        labels: Vec::new(),
        revision: 1,
    }
}

struct BlockingPort {
    entered: mpsc::Sender<()>,
    release: (Mutex<bool>, Condvar),
}
impl BlockingPort {
    fn release(&self) {
        *self.release.0.lock().unwrap() = true;
        self.release.1.notify_all();
    }
}
impl HostPorts for BlockingPort {
    fn authorized(&self, _: &Invocation, _: &str) -> bool {
        true
    }
    fn kv_get(
        &self,
        _: &Invocation,
        _: &str,
    ) -> Result<Option<plugin_host::KvValue>, plugin_api::ApiError> {
        self.entered.send(()).unwrap();
        let mut released = self.release.0.lock().unwrap();
        while !*released {
            released = self.release.1.wait(released).unwrap();
        }
        Ok(None)
    }
}

fn blocked_host(limits: Limits) -> (PluginHost, Arc<BlockingPort>, mpsc::Receiver<()>) {
    let (entered, receiver) = mpsc::channel();
    let port = Arc::new(BlockingPort {
        entered,
        release: (Mutex::new(false), Condvar::new()),
    });
    (
        host_with_ports(limits, port.clone()).unwrap(),
        port,
        receiver,
    )
}

#[test]
fn cancellation_returns_promptly_while_worker_retains_capacity() {
    let (host, port, entered) = blocked_host(Limits {
        global_active: 1,
        ..Limits::default()
    });
    let policy = host.load(component(), HOOK_WORLD).unwrap();
    let invocation = Invocation::new("blocked", Duration::from_secs(2));
    let (clone, policy_clone, invocation_clone) =
        (host.clone(), policy.clone(), invocation.clone());
    let worker = thread::spawn(move || {
        clone.invoke(
            &policy_clone,
            invocation_clone,
            batch(vec![candidate("kv")]),
        )
    });
    entered.recv_timeout(Duration::from_secs(1)).unwrap();
    let started = Instant::now();
    invocation.cancel();
    let cancelled = worker.join().unwrap();
    // Release the fake IO before assertions, even if the expected behavior is missing.
    let busy = host.invoke(
        &policy,
        Invocation::new("other", Duration::from_millis(100)),
        batch(vec![candidate("B")]),
    );
    port.release();
    assert!(started.elapsed() < Duration::from_millis(300));
    assert_eq!(cancelled.failure, Some(Failure::Cancelled));
    assert_eq!(busy.failure, Some(Failure::Busy));
}

#[test]
fn queue_wait_counts_toward_absolute_deadline() {
    let (host, port, entered) = blocked_host(Limits {
        workers: 1,
        queue: 1,
        ..Limits::default()
    });
    let policy = host.load(component(), HOOK_WORLD).unwrap();
    let (clone, policy_clone) = (host.clone(), policy.clone());
    let worker = thread::spawn(move || {
        clone.invoke(
            &policy_clone,
            Invocation::new("blocked", Duration::from_secs(1)),
            batch(vec![candidate("kv")]),
        )
    });
    entered.recv_timeout(Duration::from_secs(1)).unwrap();
    let input = vec![candidate("A")];
    let queued = host.invoke(
        &policy,
        Invocation::new("queued", Duration::from_millis(25)),
        batch(input.clone()),
    );
    port.release();
    let _ = worker.join().unwrap();
    assert_eq!(queued.failure, Some(Failure::DeadlineExceeded));
    assert_eq!(queued.candidates, input);
}

#[test]
fn epoch_interrupts_guest_and_releases_worker_without_fuel_exhaustion() {
    let limits = Limits {
        workers: 1,
        fuel: u64::MAX,
        ..Limits::default()
    };
    let host = host_with_ports(limits, Arc::new(plugin_host::NoServices)).unwrap();
    let policy = host.load(component(), HOOK_WORLD).unwrap();
    let timeout = host.invoke(
        &policy,
        Invocation::new("loop", Duration::from_millis(25)),
        batch(vec![candidate("loop")]),
    );
    assert_eq!(timeout.failure, Some(Failure::DeadlineExceeded));
    let next = host.invoke(
        &policy,
        Invocation::new("next", Duration::from_millis(250)),
        batch(vec![candidate("B")]),
    );
    assert!(next.failure.is_none(), "{:?}", next.failure);
}

#[test]
fn multiple_memories_share_one_call_budget() {
    use wasm_encoder::{
        Component, InstanceSection, MemorySection, MemoryType, Module, ModuleSection,
    };
    let modules = wasmparser::Parser::new(0)
        .parse_all(component())
        .filter(|payload| matches!(payload, Ok(wasmparser::Payload::ModuleSection { .. })))
        .count();
    let mut memories = MemorySection::new();
    for _ in 0..2 {
        memories.memory(MemoryType {
            minimum: 600,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        });
    }
    let mut module = Module::new();
    module.section(&memories);
    let mut extension = Component::new();
    extension.section(&ModuleSection(&module));
    let mut instances = InstanceSection::new();
    instances.instantiate(
        modules as u32,
        std::iter::empty::<(&str, wasm_encoder::ModuleArg)>(),
    );
    extension.section(&instances);
    let mut bytes = component().to_vec();
    bytes.extend_from_slice(&extension.finish()[8..]);
    let host = host();
    let policy = host.load(&bytes, HOOK_WORLD).unwrap();
    let outcome = host.invoke(
        &policy,
        Invocation::new("multi-memory", Duration::from_secs(1)),
        batch(vec![candidate("B")]),
    );
    assert!(
        matches!(outcome.failure, Some(Failure::Trap(ref reason))
        if reason.contains("aggregate linear memory budget")),
        "{:?}",
        outcome.failure
    );
}

#[test]
fn queue_full_is_busy_and_timed_out_worker_keeps_permit() {
    let (host, port, entered) = blocked_host(Limits {
        workers: 1,
        queue: 1,
        ..Limits::default()
    });
    let policy = host.load(component(), HOOK_WORLD).unwrap();
    let (clone, policy_clone) = (host.clone(), policy.clone());
    let first = thread::spawn(move || {
        clone.invoke(
            &policy_clone,
            Invocation::new("blocked", Duration::from_millis(25)),
            batch(vec![candidate("kv")]),
        )
    });
    entered.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(
        first.join().unwrap().failure,
        Some(Failure::DeadlineExceeded)
    );
    let (clone, policy_clone) = (host.clone(), policy.clone());
    let queued = thread::spawn(move || {
        clone.invoke(
            &policy_clone,
            Invocation::new("queued", Duration::from_millis(25)),
            batch(vec![candidate("B")]),
        )
    });
    assert_eq!(
        queued.join().unwrap().failure,
        Some(Failure::DeadlineExceeded)
    );
    // Both callers returned, but the blocked worker still occupies the sole worker.
    // Its queued cancelled job still occupies the one queue entry until that worker exits IO.
    let full = host.invoke(
        &policy,
        Invocation::new("third", Duration::from_millis(100)),
        batch(vec![candidate("B")]),
    );
    port.release();
    assert_eq!(full.failure, Some(Failure::Busy));
}

#[test]
fn per_plugin_and_total_linear_memory_admission_are_bounded() {
    for memory_only in [false, true] {
        let limits = if memory_only {
            Limits {
                total_memory_bytes: 64 * 1024 * 1024,
                ..Limits::default()
            }
        } else {
            Limits::default()
        };
        let (host, port, entered) = blocked_host(limits);
        let policy = host.load(component(), HOOK_WORLD).unwrap();
        let (clone, policy_clone) = (host.clone(), policy.clone());
        let running = thread::spawn(move || {
            clone.invoke(
                &policy_clone,
                Invocation::new("blocked", Duration::from_secs(1)),
                batch(vec![candidate("kv")]),
            )
        });
        entered.recv_timeout(Duration::from_secs(1)).unwrap();
        let blocked_plugin = if memory_only { "other" } else { "blocked" };
        let denied = host.invoke(
            &policy,
            Invocation::new(blocked_plugin, Duration::from_millis(100)),
            batch(vec![candidate("B")]),
        );
        let independent = if !memory_only {
            Some(host.invoke(
                &policy,
                Invocation::new("other", Duration::from_millis(100)),
                batch(vec![candidate("B")]),
            ))
        } else {
            None
        };
        port.release();
        assert!(running.join().unwrap().failure.is_none());
        assert_eq!(denied.failure, Some(Failure::Busy));
        if let Some(outcome) = independent {
            assert!(outcome.failure.is_none());
        }
    }
}

#[test]
fn host_facades_share_runtime_admission() {
    for quota in ["active", "plugin", "memory"] {
        let mut limits = Limits::default();
        if quota == "active" {
            limits.global_active = 1;
        }
        if quota == "memory" {
            limits.total_memory_bytes = limits.memory_bytes;
        }
        let runtime = plugin_host::PluginRuntime::new(limits).unwrap();
        let (entered, receiver) = mpsc::channel();
        let port = Arc::new(BlockingPort {
            entered,
            release: (Mutex::new(false), Condvar::new()),
        });
        let first = runtime.host(port.clone());
        let second = runtime.host(Arc::new(plugin_host::NoServices));
        let first_policy = first.load(component(), HOOK_WORLD).unwrap();
        let second_policy = second.load(component(), HOOK_WORLD).unwrap();
        let running = thread::spawn(move || {
            first.invoke(
                &first_policy,
                Invocation::new("shared", Duration::from_secs(1)),
                batch(vec![candidate("kv")]),
            )
        });
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        let plugin = if quota == "plugin" {
            "shared"
        } else {
            "independent"
        };
        let denied = second.invoke(
            &second_policy,
            Invocation::new(plugin, Duration::from_millis(100)),
            batch(vec![candidate("B")]),
        );
        port.release();
        assert!(running.join().unwrap().failure.is_none());
        assert_eq!(denied.failure, Some(Failure::Busy), "quota: {quota}");
    }
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

#[test]
fn runtime_new_rejects_zero_fuel_and_zero_background_fuel() {
    assert_eq!(
        plugin_host::PluginRuntime::new(Limits {
            fuel: 0,
            ..Limits::default()
        })
        .err(),
        Some(Failure::Unsupported)
    );
    assert_eq!(
        plugin_host::PluginRuntime::new(Limits {
            background_fuel: 0,
            ..Limits::default()
        })
        .err(),
        Some(Failure::Unsupported)
    );
}
