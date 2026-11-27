use crate::{
    CompiledEndpoint, CompiledPolicy, CompiledTask, Invocation,
    bindings::crawler::plugin::types,
    execute::{
        execute_health_indexer, execute_health_maintenance, execute_validate_indexer,
        execute_validate_maintenance,
    },
    hook_conversions::candidate_to_wit,
    ports::State,
    workers::LifecycleTarget,
};
use plugin_api::{
    Candidate, EndpointRequest, EndpointResponse, Event, Failure, FilterInput, GateInput,
    HealthState, Patch, SearchAfter, TaskInput, TaskOutcome,
};
use wasmtime::Store;

pub(crate) fn check_invocation(invocation: &Invocation) -> Result<(), Failure> {
    if invocation.expired() {
        return Err(Failure::DeadlineExceeded);
    }
    if invocation.cancelled() {
        return Err(Failure::Cancelled);
    }
    Ok(())
}

pub(crate) fn map_wasm_error(invocation: &Invocation, error: wasmtime::Error) -> Failure {
    check_invocation(invocation)
        .err()
        .unwrap_or_else(|| trap(error))
}

pub(crate) fn trap(error: wasmtime::Error) -> Failure {
    Failure::Trap(format!("{error:#}"))
}

pub(crate) fn execute_filter(
    policy: &CompiledPolicy,
    invocation: &Invocation,
    input: &SearchAfter,
    store: &mut Store<State>,
    context: types::Context,
) -> Result<Vec<types::Patch>, Failure> {
    let bindings = policy.pre.instantiate(&mut *store).map_err(trap)?;
    let candidates = input.candidates.iter().map(candidate_to_wit).collect();
    let wit_input = types::FilterInput {
        schema_version: 1,
        context,
        payload: types::FilterPayload::SearchAfter(types::SearchAfter {
            candidates,
            site_failures: input
                .site_failures
                .iter()
                .map(|failure| types::SiteFailure {
                    site_id: failure.site_id.clone(),
                    error: failure.error.clone().into(),
                })
                .collect(),
        }),
    };
    bindings
        .crawler_plugin_hooks()
        .call_filter(store, &wit_input)
        .map_err(|error| map_wasm_error(invocation, error))?
        .map_err(|error| Failure::Guest(error.into()))
}

pub(crate) fn execute_filter_typed(
    policy: &CompiledPolicy,
    invocation: &Invocation,
    input: &FilterInput,
    store: &mut Store<State>,
) -> Result<Vec<Patch>, Failure> {
    let bindings = policy.pre.instantiate(&mut *store).map_err(trap)?;
    let wit_input = types::FilterInput::from(input);
    let patches = bindings
        .crawler_plugin_hooks()
        .call_filter(store, &wit_input)
        .map_err(|error| map_wasm_error(invocation, error))?
        .map_err(|error| Failure::Guest(error.into()))?;
    Ok(patches.into_iter().map(Into::into).collect())
}

pub(crate) fn execute_gate_typed(
    policy: &CompiledPolicy,
    invocation: &Invocation,
    input: &GateInput,
    store: &mut Store<State>,
) -> Result<hooks::Gate, Failure> {
    let bindings = policy.pre.instantiate(&mut *store).map_err(trap)?;
    let wit_input = types::GateInput::from(input);
    let decision = bindings
        .crawler_plugin_hooks()
        .call_gate(store, &wit_input)
        .map_err(|error| map_wasm_error(invocation, error))?
        .map_err(|error| Failure::Guest(error.into()))?;
    Ok(decision.into())
}

pub(crate) fn execute_notify_typed(
    policy: &CompiledPolicy,
    invocation: &Invocation,
    event: &Event,
    store: &mut Store<State>,
) -> Result<(), Failure> {
    let bindings = policy.pre.instantiate(&mut *store).map_err(trap)?;
    let wit_event = types::Event::from(event);
    bindings
        .crawler_plugin_hooks()
        .call_notify(store, &wit_event)
        .map_err(|error| map_wasm_error(invocation, error))?
        .map_err(|error| Failure::Guest(error.into()))
}

pub(crate) fn execute_task(
    task: &CompiledTask,
    invocation: &Invocation,
    input: &TaskInput,
    store: &mut Store<State>,
) -> Result<TaskOutcome, Failure> {
    let bindings = task.pre.instantiate(&mut *store).map_err(trap)?;
    let wit_input = input.clone().into();
    bindings
        .crawler_plugin_tasks()
        .call_run(store, &wit_input)
        .map_err(|error| map_wasm_error(invocation, error))?
        .map(Into::into)
        .map_err(|error| Failure::Guest(error.into()))
}

pub(crate) fn execute_endpoint(
    endpoint: &CompiledEndpoint,
    invocation: &Invocation,
    request: &EndpointRequest,
    store: &mut Store<State>,
) -> Result<EndpointResponse, Failure> {
    let bindings = endpoint.pre.instantiate(&mut *store).map_err(trap)?;
    let wit_req = request.clone().into();
    bindings
        .crawler_plugin_endpoints()
        .call_handle(store, &wit_req)
        .map_err(|error| map_wasm_error(invocation, error))?
        .map(Into::into)
        .map_err(|error| Failure::Guest(error.into()))
}

pub(crate) fn execute_validate(
    target: &LifecycleTarget,
    invocation: &Invocation,
    config_schema: &str,
    state_schema: u32,
    store: &mut Store<State>,
) -> Result<(), Failure> {
    let result = match target {
        LifecycleTarget::Policy(policy) => {
            let b = policy.pre.instantiate(&mut *store).map_err(trap)?;
            b.crawler_plugin_lifecycle()
                .call_validate(store, config_schema, state_schema)
        }
        LifecycleTarget::Task(task) => {
            let b = task.pre.instantiate(&mut *store).map_err(trap)?;
            b.crawler_plugin_lifecycle()
                .call_validate(store, config_schema, state_schema)
        }
        LifecycleTarget::Endpoint(endpoint) => {
            let b = endpoint.pre.instantiate(&mut *store).map_err(trap)?;
            b.crawler_plugin_lifecycle()
                .call_validate(store, config_schema, state_schema)
        }
        LifecycleTarget::Indexer(indexer) => {
            return execute_validate_indexer(
                indexer,
                invocation,
                config_schema,
                state_schema,
                store,
            );
        }
        LifecycleTarget::Maintenance(maint) => {
            return execute_validate_maintenance(
                maint,
                invocation,
                config_schema,
                state_schema,
                store,
            );
        }
    };
    result
        .map_err(|error| map_wasm_error(invocation, error))?
        .map_err(|error| Failure::Guest(error.into()))
}

pub(crate) fn execute_health(
    target: &LifecycleTarget,
    invocation: &Invocation,
    store: &mut Store<State>,
) -> Result<HealthState, Failure> {
    match target {
        LifecycleTarget::Policy(policy) => {
            let b = policy.pre.instantiate(&mut *store).map_err(trap)?;
            b.crawler_plugin_lifecycle()
                .call_health(store)
                .map_err(|e| map_wasm_error(invocation, e))?
                .map(Into::into)
                .map_err(|e| Failure::Guest(e.into()))
        }
        LifecycleTarget::Task(task) => {
            let b = task.pre.instantiate(&mut *store).map_err(trap)?;
            b.crawler_plugin_lifecycle()
                .call_health(store)
                .map_err(|e| map_wasm_error(invocation, e))?
                .map(Into::into)
                .map_err(|e| Failure::Guest(e.into()))
        }
        LifecycleTarget::Endpoint(endpoint) => {
            let b = endpoint.pre.instantiate(&mut *store).map_err(trap)?;
            b.crawler_plugin_lifecycle()
                .call_health(store)
                .map_err(|e| map_wasm_error(invocation, e))?
                .map(Into::into)
                .map_err(|e| Failure::Guest(e.into()))
        }
        LifecycleTarget::Indexer(indexer) => execute_health_indexer(indexer, invocation, store),
        LifecycleTarget::Maintenance(maint) => execute_health_maintenance(maint, invocation, store),
    }
}

pub(crate) fn apply(
    input: &[Candidate],
    patches: Vec<types::Patch>,
) -> Result<Vec<Candidate>, Failure> {
    let mut output = input.to_vec();
    for patch in patches {
        match patch {
            types::Patch::Drop(handle) => {
                let Some(position) = output
                    .iter()
                    .position(|candidate| candidate.handle == handle)
                else {
                    return Err(Failure::InvalidPatch(
                        "unknown or already dropped handle".into(),
                    ));
                };
                output.remove(position);
            }
            types::Patch::SetOrder(handles) => {
                if handles.len() != output.len() {
                    return Err(Failure::InvalidPatch(
                        "order must contain all surviving handles".into(),
                    ));
                }
                let mut ordered = Vec::with_capacity(output.len());
                for handle in handles {
                    let Some(position) = output
                        .iter()
                        .position(|candidate| candidate.handle == handle)
                    else {
                        return Err(Failure::InvalidPatch(
                            "unknown or repeated order handle".into(),
                        ));
                    };
                    ordered.push(output.remove(position));
                }
                output = ordered;
            }
            _ => {
                return Err(Failure::InvalidPatch(
                    "unsupported patch variant for search.after".into(),
                ));
            }
        }
    }
    Ok(output)
}
