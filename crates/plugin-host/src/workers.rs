use crate::{
    HostPorts, Invocation, Limits,
    bindings::crawler::plugin::types,
    execute::{SealScope, SealedRefStore},
    limits::MemoryBudget,
    ports::State,
    workers_execute::check_invocation,
};
use plugin_api::Failure;
use std::{
    collections::HashMap,
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};
use wasmtime::{Engine, Store};

pub(crate) use crate::workers_dispatch::{Dispatch, LifecycleTarget, WorkItem};
pub(crate) use crate::workers_execute::{map_wasm_error, trap};

#[derive(Clone, Copy)]
pub(crate) enum WorkClass {
    Interactive,
    Background,
}

pub(crate) struct Channels {
    pub interactive: mpsc::SyncSender<WorkItem>,
    pub background: mpsc::SyncSender<WorkItem>,
}

#[derive(Default)]
struct Active {
    count: usize,
    memory: usize,
    plugins: HashMap<String, usize>,
    tickets: HashMap<String, usize>,
}

pub(crate) struct QueueTicket {
    core: Arc<Core>,
    plugin: String,
}

impl Drop for QueueTicket {
    fn drop(&mut self) {
        if let Ok(mut active) = self.core.active.lock()
            && let Some(count) = active.tickets.get_mut(&self.plugin)
        {
            *count -= 1;
            if *count == 0 {
                active.tickets.remove(&self.plugin);
            }
        }
        self.core.active_changed.notify_all();
    }
}

pub(crate) struct Core {
    pub engine: Arc<Engine>,
    pub limits: Limits,
    active: Mutex<Active>,
    active_changed: Condvar,
    sealed_refs: Mutex<SealedRefStore>,
}

impl Core {
    pub fn new(engine: Arc<Engine>, limits: Limits) -> Self {
        Self {
            engine,
            limits,
            active: Mutex::new(Active::default()),
            active_changed: Condvar::new(),
            sealed_refs: Mutex::new(SealedRefStore::default()),
        }
    }

    pub fn seal_download_ref(&self, scope: SealScope, raw_ref: String) -> Result<String, Failure> {
        let mut store = self
            .sealed_refs
            .lock()
            .map_err(|_| Failure::Trap("sealed ref store poisoned".into()))?;
        Ok(store.seal(scope, raw_ref))
    }

    pub fn unseal_download_ref(
        &self,
        token: &str,
        expected: &SealScope,
    ) -> Result<String, Failure> {
        let store = self
            .sealed_refs
            .lock()
            .map_err(|_| Failure::Trap("sealed ref store poisoned".into()))?;
        store.unseal(token, expected)
    }

    pub(crate) fn issue_ticket(self: &Arc<Self>, plugin: &str) -> Result<QueueTicket, Failure> {
        let mut active = self.active.lock().map_err(|_| Failure::Busy)?;
        *active.tickets.entry(plugin.into()).or_default() += 1;
        Ok(QueueTicket {
            core: self.clone(),
            plugin: plugin.into(),
        })
    }

    pub fn active_plugin_count(&self, plugin_id: &str) -> usize {
        self.active
            .lock()
            .map(|guard| guard.tickets.get(plugin_id).copied().unwrap_or(0))
            .unwrap_or(0)
    }

    pub fn wait_plugin_drained(&self, plugin_id: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let Ok(mut guard) = self.active.lock() else {
            return false;
        };
        while guard.tickets.get(plugin_id).copied().unwrap_or(0) > 0 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let Ok((next_guard, wait_res)) = self.active_changed.wait_timeout(guard, remaining)
            else {
                return false;
            };
            guard = next_guard;
            if wait_res.timed_out() && guard.tickets.get(plugin_id).copied().unwrap_or(0) > 0 {
                return false;
            }
        }
        true
    }

    pub fn start(core: &Arc<Self>) -> Result<Channels, Failure> {
        let interactive = Self::spawn_pool(
            core,
            core.limits.workers,
            core.limits.queue,
            "plugin-filter",
        )?;
        let background = Self::spawn_pool(
            core,
            core.limits.background_workers,
            core.limits.background_queue,
            "plugin-bg",
        )?;
        let engine = Arc::downgrade(&core.engine);
        thread::Builder::new()
            .name("plugin-epoch".into())
            .spawn(move || {
                loop {
                    thread::sleep(Duration::from_millis(5));
                    let Some(engine) = engine.upgrade() else {
                        break;
                    };
                    engine.increment_epoch();
                }
            })
            .map_err(|error| Failure::Trap(error.to_string()))?;
        Ok(Channels {
            interactive,
            background,
        })
    }

    fn spawn_pool(
        core: &Arc<Self>,
        workers: usize,
        queue: usize,
        prefix: &str,
    ) -> Result<mpsc::SyncSender<WorkItem>, Failure> {
        let (sender, receiver) = mpsc::sync_channel::<WorkItem>(queue);
        let receiver = Arc::new(Mutex::new(receiver));
        for index in 0..workers {
            let (core, receiver) = (core.clone(), receiver.clone());
            let worker_name = format!("{prefix}-{index}");
            thread::Builder::new()
                .name(worker_name.clone())
                .spawn(move || {
                    loop {
                        let job = match receiver.lock() {
                            Ok(receiver) => receiver.recv(),
                            Err(_) => break,
                        };
                        let Ok(job) = job else { break };
                        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let _guest = plugin_api::GuestExecutionGuard::enter();
                            core.dispatch(job);
                        }));
                        if let Err(payload) = res {
                            let reason = panic_payload_message(payload.as_ref());
                            tracing::error!(
                                worker = %worker_name,
                                reason = %reason,
                                "plugin worker caught panic during dispatch"
                            );
                        }
                    }
                })
                .map_err(|error| Failure::Trap(error.to_string()))?;
        }
        Ok(sender)
    }

    fn acquire(&self, plugin: &str, class: WorkClass) -> Result<Permit<'_>, Failure> {
        let mut active = self.active.lock().map_err(|_| Failure::Busy)?;
        let max_active = match class {
            WorkClass::Interactive => self.limits.global_active,
            WorkClass::Background => self
                .limits
                .global_active
                .saturating_sub(self.limits.effective_reserve()),
        };
        if active.count >= max_active
            || active.plugins.get(plugin).copied().unwrap_or(0) >= self.limits.per_plugin
            || active.memory + self.limits.memory_bytes > self.limits.total_memory_bytes
        {
            return Err(Failure::Busy);
        }
        active.count += 1;
        active.memory += self.limits.memory_bytes;
        *active.plugins.entry(plugin.into()).or_default() += 1;
        Ok(Permit {
            core: self,
            plugin: plugin.into(),
        })
    }

    pub(crate) fn run_scoped<T>(
        &self,
        invocation: &Invocation,
        class: WorkClass,
        fuel: u64,
        ports: &Arc<dyn HostPorts>,
        run: impl FnOnce(&mut Store<State>, types::Context) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        let context = invocation.context(ports.now_ms());
        let ctx_clone = context.clone();
        self.run_scoped_with_context(invocation, class, fuel, ports, context, |store| {
            run(store, ctx_clone)
        })
    }

    pub(crate) fn run_scoped_with_context<T>(
        &self,
        invocation: &Invocation,
        class: WorkClass,
        fuel: u64,
        ports: &Arc<dyn HostPorts>,
        context: types::Context,
        run: impl FnOnce(&mut Store<State>) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        check_invocation(invocation)?;
        let _permit = self.acquire(&invocation.plugin_id, class)?;
        let _inv_guard = ports.begin_invocation(invocation).map_err(Failure::Guest)?;
        let mut store = self.prepare_store(invocation, ports, fuel, context)?;
        let out = run(&mut store)?;
        check_invocation(invocation)?;
        Ok(out)
    }

    fn prepare_store(
        &self,
        invocation: &Invocation,
        ports: &Arc<dyn HostPorts>,
        fuel: u64,
        context: types::Context,
    ) -> Result<Store<State>, Failure> {
        let state = State {
            invocation: invocation.clone(),
            context,
            ports: ports.clone(),
            memory: MemoryBudget::new(self.limits.memory_bytes),
            http_calls_used: 0,
            browsers: HashMap::new(),
            next_browser_rep: 1,
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|state| &mut state.memory);
        store.set_fuel(fuel).map_err(trap)?;
        store.set_epoch_deadline(1);
        store.epoch_deadline_callback(|context| {
            let invocation = &context.data().invocation;
            if invocation.expired() || invocation.cancelled() {
                return Err(wasmtime::Error::msg("invocation deadline/cancellation"));
            }
            Ok(wasmtime::UpdateDeadline::Continue(1))
        });
        Ok(store)
    }
}

struct Permit<'a> {
    core: &'a Core,
    plugin: String,
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.core.active.lock() {
            active.count -= 1;
            active.memory -= self.core.limits.memory_bytes;
            if let Some(count) = active.plugins.get_mut(&self.plugin) {
                *count -= 1;
                if *count == 0 {
                    active.plugins.remove(&self.plugin);
                }
            }
        }
        self.core.active_changed.notify_all();
    }
}

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> &str {
    if let Some(msg) = payload.downcast_ref::<&'static str>() {
        msg
    } else if let Some(msg) = payload.downcast_ref::<String>() {
        msg.as_str()
    } else {
        "non-string panic payload"
    }
}
