use crate::{
    HostPorts, Invocation, PluginRuntime,
    bindings::{
        HookPluginPre,
        crawler::plugin,
        endpoint_world::EndpointPluginPre,
        indexer_world,
        task_world::{self, TaskPluginPre},
    },
    ports::State,
    workers::{Core, Dispatch, LifecycleTarget, QueueTicket, WorkClass, WorkItem},
};
use plugin_api::{
    EndpointRequest, EndpointResponse, Event, Failure, FilterInput, FilterOutcome, GuestRole,
    HealthState, Patch, SearchAfter, TaskInput, TaskOutcome,
};
use std::{
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};
use wasmtime::{
    Engine,
    component::{Component, HasSelf, InstancePre, Linker},
};

pub use plugin_api::{ENDPOINT_WORLD, HOOK_WORLD, TASK_WORLD};

const HOOK_IMPORTS: &[&str] = &[
    "types",
    "runtime",
    "clock",
    "config",
    "log",
    "kv",
    "blob",
    "site",
    "subscribe",
    "library",
    "download",
    "history",
    "catalog",
    "operations",
    "http",
    "notify",
];
const ACTION_IMPORTS: &[&str] = &[
    "types",
    "runtime",
    "clock",
    "config",
    "log",
    "kv",
    "blob",
    "site",
    "subscribe",
    "library",
    "download",
    "history",
    "catalog",
    "operations",
    "http",
    "commands",
    "boost",
    "indexer-cache",
];

#[derive(Clone)]
pub struct CompiledPolicy {
    pub(crate) pre: HookPluginPre<State>,
}

#[derive(Clone)]
pub struct CompiledTask {
    pub(crate) pre: TaskPluginPre<State>,
}

#[derive(Clone)]
pub struct CompiledEndpoint {
    pub(crate) pre: EndpointPluginPre<State>,
}

#[derive(Clone)]
pub struct PluginHost {
    pub(crate) core: Arc<Core>,
    pub(crate) interactive: mpsc::SyncSender<WorkItem>,
    pub(crate) background: mpsc::SyncSender<WorkItem>,
    pub(crate) ports: Arc<dyn HostPorts>,
}

impl PluginHost {
    pub(crate) fn from_runtime(runtime: &PluginRuntime, ports: Arc<dyn HostPorts>) -> Self {
        Self {
            core: runtime.core.clone(),
            interactive: runtime.interactive.clone(),
            background: runtime.background.clone(),
            ports,
        }
    }

    pub fn active_plugin_count(&self, plugin_id: &str) -> usize {
        self.core.active_plugin_count(plugin_id)
    }

    pub fn wait_plugin_drained(&self, plugin_id: &str, timeout: Duration) -> bool {
        self.core.wait_plugin_drained(plugin_id, timeout)
    }

    /// Compilation is installation work; invocation budgets begin at submission.
    pub fn load(&self, bytes: &[u8], world: &str) -> Result<CompiledPolicy, Failure> {
        if world != HOOK_WORLD {
            return Err(Failure::InvalidComponent("unsupported exact world".into()));
        }
        let instance =
            self.instantiate_checked(bytes, HOOK_IMPORTS, "hooks", &["filter", "gate", "notify"])?;
        let pre = HookPluginPre::new(instance).map_err(invalid)?;
        Ok(CompiledPolicy { pre })
    }

    /// Compilation is installation work; invocation budgets begin at submission.
    pub fn load_task(&self, bytes: &[u8], world: &str) -> Result<CompiledTask, Failure> {
        if world != TASK_WORLD {
            return Err(Failure::InvalidComponent("unsupported exact world".into()));
        }
        let instance = self.instantiate_checked(bytes, ACTION_IMPORTS, "tasks", &["run"])?;
        let pre = TaskPluginPre::new(instance).map_err(invalid)?;
        Ok(CompiledTask { pre })
    }

    /// Compilation is installation work; invocation budgets begin at submission.
    pub fn load_endpoint(&self, bytes: &[u8], world: &str) -> Result<CompiledEndpoint, Failure> {
        if world != ENDPOINT_WORLD {
            return Err(Failure::InvalidComponent("unsupported exact world".into()));
        }
        let instance = self.instantiate_checked(bytes, ACTION_IMPORTS, "endpoints", &["handle"])?;
        let pre = EndpointPluginPre::new(instance).map_err(invalid)?;
        Ok(CompiledEndpoint { pre })
    }

    pub(crate) fn instantiate_checked(
        &self,
        bytes: &[u8],
        allowed_imports: &[&str],
        primary_export: &str,
        primary_funcs: &[&str],
    ) -> Result<InstancePre<State>, Failure> {
        let component = Component::new(&self.core.engine, bytes).map_err(invalid)?;
        validate_component(
            &self.core.engine,
            &component,
            allowed_imports,
            primary_export,
            primary_funcs,
        )?;
        let linker = self.linker().map_err(invalid)?;
        linker.instantiate_pre(&component).map_err(invalid)
    }

    fn linker(&self) -> wasmtime::Result<Linker<State>> {
        let mut linker = Linker::new(&self.core.engine);
        plugin::runtime::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        plugin::clock::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        plugin::config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        plugin::log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        plugin::kv::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        plugin::blob::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        plugin::operations::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        plugin::http::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        plugin::site::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        plugin::subscribe::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        plugin::library::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        plugin::download::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        plugin::history::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        plugin::catalog::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        plugin::notify::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        task_world::crawler::plugin::commands::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        task_world::crawler::plugin::boost::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        task_world::crawler::plugin::indexer_cache::add_to_linker::<_, HasSelf<_>>(
            &mut linker,
            |s| s,
        )?;
        indexer_world::crawler::plugin::browser::add_to_linker::<_, HasSelf<_>>(
            &mut linker,
            |s| s,
        )?;
        Ok(linker)
    }

    pub fn invoke(
        &self,
        policy: &CompiledPolicy,
        mut invocation: Invocation,
        input: SearchAfter,
    ) -> FilterOutcome {
        if plugin_api::in_guest_execution() {
            return failed(input, Failure::Reentrant);
        }
        if let Err(failure) = crate::validation::validate(&input) {
            return failed(input, failure);
        }
        if !Engine::same(policy.pre.engine(), &self.core.engine) {
            return failed(
                input,
                Failure::InvalidComponent("component belongs to another runtime".into()),
            );
        }
        invocation.role = GuestRole::Filter;
        let original = input.clone();
        let policy = policy.clone();
        match self.submit_wait(
            &self.interactive,
            invocation,
            self.core.limits.wall,
            |inv, reply, ticket| WorkItem {
                ticket,
                class: WorkClass::Interactive,
                fuel: self.core.limits.fuel,
                invocation: inv,
                ports: self.ports.clone(),
                dispatch: Dispatch::Filter {
                    policy,
                    input,
                    reply,
                },
            },
        ) {
            Ok(outcome) => outcome,
            Err(failure) => failed(original, failure),
        }
    }

    pub fn invoke_filter(
        &self,
        policy: &CompiledPolicy,
        mut invocation: Invocation,
        mut input: FilterInput,
    ) -> Result<Vec<Patch>, Failure> {
        self.check_ready(policy.pre.engine())?;
        if input.schema_version != 1 {
            return Err(Failure::InvalidInput(
                "unsupported filter schema_version".into(),
            ));
        }
        invocation.role = GuestRole::Filter;
        propagate_hook_context(&mut invocation, &input.context);
        input.context.invocation_id = invocation.id().to_string();
        let policy = policy.clone();
        self.submit_wait(
            &self.interactive,
            invocation,
            self.core.limits.wall,
            |inv, reply, ticket| WorkItem {
                ticket,
                class: WorkClass::Interactive,
                fuel: self.core.limits.fuel,
                invocation: inv,
                ports: self.ports.clone(),
                dispatch: Dispatch::FilterTyped {
                    policy,
                    input,
                    reply,
                },
            },
        )?
    }

    pub fn invoke_gate(
        &self,
        policy: &CompiledPolicy,
        mut invocation: Invocation,
        mut input: plugin_api::GateInput,
    ) -> Result<hooks::Gate, Failure> {
        self.check_ready(policy.pre.engine())?;
        if input.schema_version != 1 {
            return Err(Failure::InvalidInput(
                "unsupported gate schema_version".into(),
            ));
        }
        invocation.role = GuestRole::Gate;
        propagate_hook_context(&mut invocation, &input.context);
        input.context.invocation_id = invocation.id().to_string();
        let policy = policy.clone();
        self.submit_wait(
            &self.interactive,
            invocation,
            self.core.limits.wall,
            |inv, reply, ticket| WorkItem {
                ticket,
                class: WorkClass::Interactive,
                fuel: self.core.limits.fuel,
                invocation: inv,
                ports: self.ports.clone(),
                dispatch: Dispatch::GateTyped {
                    policy,
                    input,
                    reply,
                },
            },
        )?
    }

    pub fn invoke_notify(
        &self,
        policy: &CompiledPolicy,
        mut invocation: Invocation,
        mut event: Event,
    ) -> Result<(), Failure> {
        self.check_ready(policy.pre.engine())?;
        if event.schema_version != 1 {
            return Err(Failure::InvalidInput(
                "unsupported event schema_version".into(),
            ));
        }
        if event.id.trim().is_empty() {
            return Err(Failure::InvalidInput("event id must not be empty".into()));
        }
        if event.kind.trim().is_empty() {
            return Err(Failure::InvalidInput("event kind must not be empty".into()));
        }
        invocation.role = GuestRole::Notify;
        propagate_hook_context(&mut invocation, &event.context);
        event.context.invocation_id = invocation.id().to_string();
        let policy = policy.clone();
        self.submit_wait(
            &self.interactive,
            invocation,
            self.core.limits.wall,
            |inv, reply, ticket| WorkItem {
                ticket,
                class: WorkClass::Interactive,
                fuel: self.core.limits.fuel,
                invocation: inv,
                ports: self.ports.clone(),
                dispatch: Dispatch::NotifyTyped {
                    policy,
                    event,
                    reply,
                },
            },
        )?
    }

    pub fn invoke_task(
        &self,
        task: &CompiledTask,
        mut invocation: Invocation,
        input: TaskInput,
    ) -> Result<TaskOutcome, Failure> {
        self.check_ready(task.pre.engine())?;
        invocation.role = GuestRole::Task;
        let task = task.clone();
        self.submit_wait(
            &self.background,
            invocation,
            self.core.limits.task_wall,
            |inv, reply, ticket| WorkItem {
                ticket,
                class: WorkClass::Background,
                fuel: self.core.limits.background_fuel,
                invocation: inv,
                ports: self.ports.clone(),
                dispatch: Dispatch::Task { task, input, reply },
            },
        )?
    }

    pub fn invoke_endpoint(
        &self,
        endpoint: &CompiledEndpoint,
        mut invocation: Invocation,
        request: EndpointRequest,
    ) -> Result<EndpointResponse, Failure> {
        self.check_ready(endpoint.pre.engine())?;
        invocation.role = GuestRole::Endpoint;
        let endpoint = endpoint.clone();
        self.submit_wait(
            &self.interactive,
            invocation,
            self.core.limits.endpoint_wall,
            |inv, reply, ticket| WorkItem {
                ticket,
                class: WorkClass::Interactive,
                fuel: self.core.limits.background_fuel,
                invocation: inv,
                ports: self.ports.clone(),
                dispatch: Dispatch::Endpoint {
                    endpoint,
                    request,
                    reply,
                },
            },
        )?
    }

    pub fn validate_policy(
        &self,
        policy: &CompiledPolicy,
        invocation: Invocation,
        config_schema: &str,
        state_schema: u32,
    ) -> Result<(), Failure> {
        self.validate_target(
            policy.pre.engine(),
            LifecycleTarget::Policy(policy.clone()),
            invocation,
            config_schema,
            state_schema,
        )
    }

    pub fn validate_task(
        &self,
        task: &CompiledTask,
        invocation: Invocation,
        config_schema: &str,
        state_schema: u32,
    ) -> Result<(), Failure> {
        self.validate_target(
            task.pre.engine(),
            LifecycleTarget::Task(task.clone()),
            invocation,
            config_schema,
            state_schema,
        )
    }

    pub fn validate_endpoint(
        &self,
        endpoint: &CompiledEndpoint,
        invocation: Invocation,
        config_schema: &str,
        state_schema: u32,
    ) -> Result<(), Failure> {
        self.validate_target(
            endpoint.pre.engine(),
            LifecycleTarget::Endpoint(endpoint.clone()),
            invocation,
            config_schema,
            state_schema,
        )
    }

    pub fn health_policy(
        &self,
        policy: &CompiledPolicy,
        invocation: Invocation,
    ) -> Result<HealthState, Failure> {
        self.health_target(
            policy.pre.engine(),
            LifecycleTarget::Policy(policy.clone()),
            invocation,
        )
    }

    pub fn health_task(
        &self,
        task: &CompiledTask,
        invocation: Invocation,
    ) -> Result<HealthState, Failure> {
        self.health_target(
            task.pre.engine(),
            LifecycleTarget::Task(task.clone()),
            invocation,
        )
    }

    pub fn health_endpoint(
        &self,
        endpoint: &CompiledEndpoint,
        invocation: Invocation,
    ) -> Result<HealthState, Failure> {
        self.health_target(
            endpoint.pre.engine(),
            LifecycleTarget::Endpoint(endpoint.clone()),
            invocation,
        )
    }

    pub fn filter(&self, component: &[u8], input: SearchAfter) -> FilterOutcome {
        if plugin_api::in_guest_execution() {
            return failed(input, Failure::Reentrant);
        }
        match self.load(component, HOOK_WORLD) {
            Ok(policy) => self.invoke(
                &policy,
                Invocation::new("policy", self.core.limits.wall),
                input,
            ),
            Err(error) => failed(input, error),
        }
    }

    pub(crate) fn validate_target(
        &self,
        engine: &Engine,
        target: LifecycleTarget,
        mut invocation: Invocation,
        config_schema: &str,
        state_schema: u32,
    ) -> Result<(), Failure> {
        self.check_ready(engine)?;
        invocation.role = GuestRole::Lifecycle;
        let schema = config_schema.to_string();
        self.submit_wait(
            &self.interactive,
            invocation,
            self.core.limits.wall,
            |inv, reply, ticket| WorkItem {
                ticket,
                class: WorkClass::Interactive,
                fuel: self.core.limits.fuel,
                invocation: inv,
                ports: self.ports.clone(),
                dispatch: Dispatch::Validate {
                    target,
                    config_schema: schema,
                    state_schema,
                    reply,
                },
            },
        )?
    }

    pub(crate) fn health_target(
        &self,
        engine: &Engine,
        target: LifecycleTarget,
        mut invocation: Invocation,
    ) -> Result<HealthState, Failure> {
        self.check_ready(engine)?;
        invocation.role = GuestRole::Lifecycle;
        self.submit_wait(
            &self.interactive,
            invocation,
            self.core.limits.wall,
            |inv, reply, ticket| WorkItem {
                ticket,
                class: WorkClass::Interactive,
                fuel: self.core.limits.fuel,
                invocation: inv,
                ports: self.ports.clone(),
                dispatch: Dispatch::Health { target, reply },
            },
        )?
    }

    pub(crate) fn check_ready(&self, component_engine: &Engine) -> Result<(), Failure> {
        if plugin_api::in_guest_execution() {
            return Err(Failure::Reentrant);
        }
        if !Engine::same(component_engine, &self.core.engine) {
            return Err(Failure::InvalidComponent(
                "component belongs to another runtime".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn submit_wait<T: Send + 'static>(
        &self,
        sender: &mpsc::SyncSender<WorkItem>,
        mut invocation: Invocation,
        wall: Duration,
        build: impl FnOnce(Invocation, mpsc::SyncSender<T>, QueueTicket) -> WorkItem,
    ) -> Result<T, Failure> {
        invocation.deadline = invocation.deadline.min(Instant::now() + wall);
        invocation.max_http_calls = invocation
            .max_http_calls
            .min(self.core.limits.max_http_calls);
        if invocation.cancelled() {
            return Err(Failure::Cancelled);
        }
        if invocation.expired() {
            return Err(Failure::DeadlineExceeded);
        }
        let ticket = self.core.issue_ticket(&invocation.plugin_id)?;
        let (tx, rx) = mpsc::sync_channel(1);
        if sender
            .try_send(build(invocation.clone(), tx, ticket))
            .is_err()
        {
            return Err(Failure::Busy);
        }
        loop {
            if invocation.cancelled() {
                return Err(Failure::Cancelled);
            }
            if invocation.expired() {
                invocation.cancel();
                return Err(Failure::DeadlineExceeded);
            }
            let wait = invocation
                .deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(5));
            match rx.recv_timeout(wait) {
                Ok(result) => {
                    if invocation.expired() {
                        return Err(Failure::DeadlineExceeded);
                    }
                    if invocation.cancelled() {
                        return Err(Failure::Cancelled);
                    }
                    return Ok(result);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(Failure::Trap("worker exited unexpectedly".into()));
                }
            }
        }
    }
}

fn validate_component(
    engine: &Engine,
    component: &Component,
    allowed_imports: &[&str],
    primary_export: &str,
    primary_funcs: &[&str],
) -> Result<(), Failure> {
    let ty = component.component_type();
    for (name, _) in ty.imports(engine) {
        if !allowed_imports
            .iter()
            .any(|interface| name == format!("crawler:plugin/{interface}@1.0.0"))
        {
            return Err(Failure::InvalidComponent(format!(
                "unsupported import: {name}"
            )));
        }
    }
    let primary_iface = format!("crawler:plugin/{primary_export}@1.0.0");
    for (name, item) in ty.exports(engine) {
        if !["types", primary_export, "lifecycle"]
            .iter()
            .any(|interface| name == format!("crawler:plugin/{interface}@1.0.0"))
        {
            return Err(Failure::InvalidComponent(format!(
                "unexpected export: {name}"
            )));
        }
        if let wasmtime::component::types::ComponentItem::ComponentInstance(instance) = item.ty {
            for (function, item) in instance.exports(engine) {
                if matches!(
                    item.ty,
                    wasmtime::component::types::ComponentItem::ComponentFunc(_)
                ) {
                    let allowed: &[&str] = if name == primary_iface {
                        primary_funcs
                    } else if name == "crawler:plugin/lifecycle@1.0.0" {
                        &["validate", "health"]
                    } else {
                        &[]
                    };
                    if !allowed.contains(&function) {
                        return Err(Failure::InvalidComponent(format!(
                            "unexpected export: {name}#{function}"
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn failed(input: SearchAfter, failure: Failure) -> FilterOutcome {
    tracing::warn!(error = %failure, "plugin filter failed; retaining input");
    FilterOutcome {
        candidates: input.candidates,
        site_failures: input.site_failures,
        failure: Some(failure),
    }
}

pub(crate) fn invalid(error: wasmtime::Error) -> Failure {
    Failure::InvalidComponent(format!("{error:#}"))
}

fn propagate_hook_context(invocation: &mut Invocation, ctx: &plugin_api::HookContext) {
    if crate::http_site_ports::canonical_site_id(invocation.site_id.as_deref()).is_none() {
        invocation.site_id =
            crate::http_site_ports::canonical_site_id(ctx.site_id.as_deref()).map(str::to_string);
    }
    if invocation.subscribe_id.is_none() {
        invocation.subscribe_id = ctx.subscribe_id.clone();
    }
    if invocation.library_id.is_none() {
        invocation.library_id = ctx.library_id.clone();
    }
    if !ctx.subject.is_empty() && invocation.subject == "system" {
        invocation.subject = ctx.subject.clone();
    }
    if !ctx.cause_id.is_empty() && invocation.cause_id == invocation.id {
        invocation.cause_id = ctx.cause_id.clone();
    }
    if ctx.cancelled {
        invocation.cancel();
    }
}
