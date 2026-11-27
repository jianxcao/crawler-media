mod admin_command_handles;
mod admin_ops;
mod admin_queries;
pub(crate) mod boost;
mod browser_adapter;
mod browser_check;
mod browser_html;
pub(crate) mod builtins;
pub(crate) mod command_targets;
mod commands;
mod component_cache;
mod config_ops;
mod dev_sideload;
pub(crate) mod effect;
pub mod entrypoints;
mod errors;
pub(crate) mod event_builders;
pub(crate) mod events;
mod events_delivery;
pub(crate) mod hooks_catalog_and_traces;
#[doc(hidden)]
pub mod hooks_runtime;
mod idempotency;
mod in_flight;
mod install_ops;
mod lifecycle_ops;
pub(crate) mod op_settlement;
mod policy_ops;
mod ports;
mod reauth;
mod reconcile;
mod recovery;
mod recovery_lifecycle;
mod recovery_rollforward;
mod registry_sync;
mod service;
mod service_exec;
mod staging;
mod state_reset_ops;
mod task_ops;
mod tasks;
pub(crate) mod transfer_rename;
mod types;
mod ui_asset_io;
mod ui_builtins;
mod ui_builtins_assets;
mod ui_handles;
mod ui_indexer;
mod ui_ops;
mod ui_queries;
mod ui_query_readers;
mod ui_trust_ops;
mod upgrade_ops;
mod upgrade_switch;
mod upgrade_switch_exec;
mod wasm_runner;

pub(crate) use browser_check::RunBrowserCheckInput;
pub use commands::{run_command_job, submit_command};
pub use effect::{
    ExternalEffectStep, PluginEffectAdapter, PluginEffectContext, PluginInvocationRole,
    PluginTaskInvocation, PluginTaskRunner,
};
#[doc(hidden)]
pub use op_settlement::fail_staged_operation;
pub(crate) use ports::HostDeps;
pub(crate) use reconcile::reconcile_pending_in_loop;
pub use reconcile::{reconcile_operation, reconcile_pending, recover_startup, run_reconcile_job};
#[doc(hidden)]
pub use registry_sync::sync_registry_snapshot;
pub(crate) use registry_sync::sync_registry_snapshot_on_startup;
pub use service::{PluginService, SubmitPluginCommand};
pub use tasks::run_task_job;
pub use types::*;
pub use ui_ops::*;
pub use ui_trust_ops::*;
pub(crate) use wasm_runner::resolve_plugin_trust;
