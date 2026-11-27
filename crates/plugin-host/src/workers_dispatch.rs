use crate::{
    CompiledEndpoint, CompiledIndexer, CompiledMaintenance, CompiledPolicy, CompiledTask,
    HostPorts, Invocation,
    bindings::crawler::plugin::types,
    engine::failed,
    execute::{
        execute_indexer_categories, execute_indexer_describe, execute_indexer_detail,
        execute_indexer_resolve_download, execute_indexer_rss, execute_indexer_search,
        execute_indexer_user_stats, execute_maintenance_run, seal_scope, validate_torrent_raw,
    },
    workers::Core,
    workers_execute::{
        apply, execute_endpoint, execute_filter, execute_filter_typed, execute_gate_typed,
        execute_health, execute_notify_typed, execute_task, execute_validate,
    },
};
use plugin_api::{
    Category, Descriptor, EndpointRequest, EndpointResponse, Event, Failure, FilterInput,
    FilterOutcome, GateInput, HealthState, HttpRequest, MaintenanceAction, MaintenanceOutcome,
    Patch, SearchAfter, SearchQuery, StatsView, TaskInput, TaskOutcome, TorrentBatch, TorrentRaw,
};
use std::sync::{Arc, mpsc};

use crate::workers::{QueueTicket, WorkClass};

#[derive(Clone)]
pub(crate) enum LifecycleTarget {
    Policy(CompiledPolicy),
    Task(CompiledTask),
    Endpoint(CompiledEndpoint),
    Indexer(CompiledIndexer),
    Maintenance(CompiledMaintenance),
}

pub(crate) enum Dispatch {
    Filter {
        policy: CompiledPolicy,
        input: SearchAfter,
        reply: mpsc::SyncSender<FilterOutcome>,
    },
    FilterTyped {
        policy: CompiledPolicy,
        input: FilterInput,
        reply: mpsc::SyncSender<Result<Vec<Patch>, Failure>>,
    },
    GateTyped {
        policy: CompiledPolicy,
        input: GateInput,
        reply: mpsc::SyncSender<Result<hooks::Gate, Failure>>,
    },
    NotifyTyped {
        policy: CompiledPolicy,
        event: Event,
        reply: mpsc::SyncSender<Result<(), Failure>>,
    },
    Task {
        task: CompiledTask,
        input: TaskInput,
        reply: mpsc::SyncSender<Result<TaskOutcome, Failure>>,
    },
    Endpoint {
        endpoint: CompiledEndpoint,
        request: EndpointRequest,
        reply: mpsc::SyncSender<Result<EndpointResponse, Failure>>,
    },
    IndexerDescribe {
        indexer: CompiledIndexer,
        reply: mpsc::SyncSender<Result<Vec<Descriptor>, Failure>>,
    },
    IndexerSearch {
        indexer: CompiledIndexer,
        provider_id: String,
        query: SearchQuery,
        reply: mpsc::SyncSender<Result<TorrentBatch, Failure>>,
    },
    IndexerRss {
        indexer: CompiledIndexer,
        provider_id: String,
        cursor: Option<String>,
        reply: mpsc::SyncSender<Result<TorrentBatch, Failure>>,
    },
    IndexerDetail {
        indexer: CompiledIndexer,
        provider_id: String,
        torrent_ref: String,
        reply: mpsc::SyncSender<Result<TorrentRaw, Failure>>,
    },
    IndexerResolveDownload {
        indexer: CompiledIndexer,
        provider_id: String,
        download_ref: String,
        reply: mpsc::SyncSender<Result<(HttpRequest, bool), Failure>>,
    },
    IndexerUserStats {
        indexer: CompiledIndexer,
        reply: mpsc::SyncSender<Result<StatsView, Failure>>,
    },
    IndexerCategories {
        indexer: CompiledIndexer,
        reply: mpsc::SyncSender<Result<Vec<Category>, Failure>>,
    },
    MaintenanceRun {
        maintenance: CompiledMaintenance,
        action: MaintenanceAction,
        job_id: String,
        reply: mpsc::SyncSender<Result<MaintenanceOutcome, Failure>>,
    },
    Validate {
        target: LifecycleTarget,
        config_schema: String,
        state_schema: u32,
        reply: mpsc::SyncSender<Result<(), Failure>>,
    },
    Health {
        target: LifecycleTarget,
        reply: mpsc::SyncSender<Result<HealthState, Failure>>,
    },
}

pub(crate) struct WorkItem {
    pub ticket: QueueTicket,
    pub class: WorkClass,
    pub fuel: u64,
    pub invocation: Invocation,
    pub ports: Arc<dyn HostPorts>,
    pub dispatch: Dispatch,
}

impl Core {
    pub(super) fn dispatch(&self, job: WorkItem) {
        let WorkItem {
            ticket: _ticket,
            class,
            fuel,
            invocation,
            ports,
            dispatch,
        } = job;
        match dispatch {
            Dispatch::Filter {
                policy,
                input,
                reply,
            } => {
                let outcome =
                    match self.run_scoped(&invocation, class, fuel, &ports, |store, ctx| {
                        execute_filter(&policy, &invocation, &input, store, ctx)
                    }) {
                        Ok(patches) => match apply(&input.candidates, patches) {
                            Ok(candidates) => FilterOutcome {
                                candidates,
                                site_failures: input.site_failures,
                                failure: None,
                            },
                            Err(failure) => failed(input, failure),
                        },
                        Err(failure) => failed(input, failure),
                    };
                let _ = reply.send(outcome);
            }
            Dispatch::FilterTyped {
                policy,
                mut input,
                reply,
            } => {
                align_hook_context(&invocation, &mut input.context, ports.now_ms());
                let res = self.run_scoped_with_context(
                    &invocation,
                    class,
                    fuel,
                    &ports,
                    types::Context::from(&input.context),
                    |store| execute_filter_typed(&policy, &invocation, &input, store),
                );
                let _ = reply.send(res);
            }
            Dispatch::GateTyped {
                policy,
                mut input,
                reply,
            } => {
                align_hook_context(&invocation, &mut input.context, ports.now_ms());
                let res = self.run_scoped_with_context(
                    &invocation,
                    class,
                    fuel,
                    &ports,
                    types::Context::from(&input.context),
                    |store| execute_gate_typed(&policy, &invocation, &input, store),
                );
                let _ = reply.send(res);
            }
            Dispatch::NotifyTyped {
                policy,
                mut event,
                reply,
            } => {
                align_hook_context(&invocation, &mut event.context, ports.now_ms());
                let res = self.run_scoped_with_context(
                    &invocation,
                    class,
                    fuel,
                    &ports,
                    types::Context::from(&event.context),
                    |store| execute_notify_typed(&policy, &invocation, &event, store),
                );
                let _ = reply.send(res);
            }
            Dispatch::Task { task, input, reply } => {
                let res = self.run_scoped(&invocation, class, fuel, &ports, |store, _| {
                    execute_task(&task, &invocation, &input, store)
                });
                let _ = reply.send(res);
            }
            Dispatch::Endpoint {
                endpoint,
                request,
                reply,
            } => {
                let res = self.run_scoped(&invocation, class, fuel, &ports, |store, _| {
                    execute_endpoint(&endpoint, &invocation, &request, store)
                });
                let _ = reply.send(res);
            }
            Dispatch::Validate {
                target,
                config_schema,
                state_schema,
                reply,
            } => {
                let res = self.run_scoped(&invocation, class, fuel, &ports, |store, _| {
                    execute_validate(&target, &invocation, &config_schema, state_schema, store)
                });
                let _ = reply.send(res);
            }
            Dispatch::Health { target, reply } => {
                let res = self.run_scoped(&invocation, class, fuel, &ports, |store, _| {
                    execute_health(&target, &invocation, store)
                });
                let _ = reply.send(res);
            }
            other => self.dispatch_provider(class, fuel, &invocation, &ports, other),
        }
    }

    fn dispatch_provider(
        &self,
        class: WorkClass,
        fuel: u64,
        invocation: &Invocation,
        ports: &Arc<dyn HostPorts>,
        dispatch: Dispatch,
    ) {
        match dispatch {
            Dispatch::IndexerDescribe { indexer, reply } => {
                let res = self.run_scoped(invocation, class, fuel, ports, |store, _| {
                    execute_indexer_describe(&indexer, invocation, store)
                });
                let _ = reply.send(res);
            }
            Dispatch::IndexerSearch {
                indexer,
                provider_id,
                query,
                reply,
            } => {
                let res = self
                    .run_scoped(invocation, class, fuel, ports, |store, _| {
                        execute_indexer_search(&indexer, invocation, &query, store)
                    })
                    .and_then(|batch| self.seal_batch(invocation, &provider_id, batch));
                let _ = reply.send(res);
            }
            Dispatch::IndexerRss {
                indexer,
                provider_id,
                cursor,
                reply,
            } => {
                let res = self
                    .run_scoped(invocation, class, fuel, ports, |store, _| {
                        execute_indexer_rss(&indexer, invocation, cursor.as_deref(), store)
                    })
                    .and_then(|batch| self.seal_batch(invocation, &provider_id, batch));
                let _ = reply.send(res);
            }
            Dispatch::IndexerDetail {
                indexer,
                provider_id,
                torrent_ref,
                reply,
            } => {
                let res = self
                    .run_scoped(invocation, class, fuel, ports, |store, _| {
                        execute_indexer_detail(&indexer, invocation, &torrent_ref, store)
                    })
                    .and_then(|item| self.seal_item(invocation, &provider_id, item));
                let _ = reply.send(res);
            }
            Dispatch::IndexerResolveDownload {
                indexer,
                provider_id,
                download_ref,
                reply,
            } => {
                let scope = seal_scope(invocation, &provider_id);
                let res = self
                    .unseal_download_ref(&download_ref, &scope)
                    .and_then(|raw| {
                        self.run_scoped(invocation, class, fuel, ports, |store, _| {
                            let req = execute_indexer_resolve_download(
                                &indexer, invocation, &raw, store,
                            )?;
                            let site_auth_granted =
                                store.data().ports.authorized(invocation, "http:site-auth");
                            Ok((req, site_auth_granted))
                        })
                    });
                let _ = reply.send(res);
            }
            Dispatch::IndexerUserStats { indexer, reply } => {
                let res = self.run_scoped(invocation, class, fuel, ports, |store, _| {
                    execute_indexer_user_stats(&indexer, invocation, store)
                });
                let _ = reply.send(res);
            }
            Dispatch::IndexerCategories { indexer, reply } => {
                let res = self.run_scoped(invocation, class, fuel, ports, |store, _| {
                    execute_indexer_categories(&indexer, invocation, store)
                });
                let _ = reply.send(res);
            }
            Dispatch::MaintenanceRun {
                maintenance,
                action,
                job_id,
                reply,
            } => {
                let res = self.run_scoped(invocation, class, fuel, ports, |store, _| {
                    execute_maintenance_run(&maintenance, invocation, action, &job_id, store)
                });
                let _ = reply.send(res);
            }
            Dispatch::Filter { .. }
            | Dispatch::FilterTyped { .. }
            | Dispatch::GateTyped { .. }
            | Dispatch::NotifyTyped { .. }
            | Dispatch::Task { .. }
            | Dispatch::Endpoint { .. }
            | Dispatch::Validate { .. }
            | Dispatch::Health { .. } => {}
        }
    }

    fn seal_item(
        &self,
        invocation: &Invocation,
        provider_id: &str,
        mut item: TorrentRaw,
    ) -> Result<TorrentRaw, Failure> {
        validate_torrent_raw(&item)?;
        let scope = seal_scope(invocation, provider_id);
        item.download_ref = self.seal_download_ref(scope, item.download_ref)?;
        Ok(item)
    }

    fn seal_batch(
        &self,
        invocation: &Invocation,
        provider_id: &str,
        mut batch: TorrentBatch,
    ) -> Result<TorrentBatch, Failure> {
        for item in &mut batch.items {
            validate_torrent_raw(item)?;
            let scope = seal_scope(invocation, provider_id);
            let raw = std::mem::take(&mut item.download_ref);
            item.download_ref = self.seal_download_ref(scope, raw)?;
        }
        Ok(batch)
    }
}

fn align_hook_context(inv: &Invocation, ctx: &mut plugin_api::HookContext, now_ms: u64) {
    let inv_ctx = inv.context(now_ms);
    ctx.invocation_id = inv_ctx.invocation_id;
    ctx.cause_id = inv_ctx.cause_id;
    ctx.plugin_id = inv_ctx.plugin_id;
    ctx.generation = inv_ctx.generation;
    ctx.config_revision = inv_ctx.config_revision;
    ctx.subject = inv_ctx.subject;
    ctx.site_id = inv_ctx.site_id;
    ctx.subscribe_id = inv_ctx.subscribe_id;
    ctx.library_id = inv_ctx.library_id;
    ctx.deadline_ms = inv_ctx.deadline_ms;
    ctx.cancelled = inv_ctx.cancelled;
}
