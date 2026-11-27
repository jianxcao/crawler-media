//! Authoritative plugin metadata and durable host intents. No package or runtime IO.
mod admin_reads;
mod audit;
mod authority;
mod config;
mod dev_reload;
mod developer_mode;
mod grants;
mod idempotency;
mod lifecycle;
pub(crate) mod paging;
mod quota;
pub(crate) use quota::purge_retired_site_scopes;
mod records;
mod secrets;
mod staged_accept;
mod staged_operations;
mod state;
mod state_types;
mod trust_keys;
mod ui_trust;
mod upgrade;
pub use admin_reads::{
    PluginAuditEvent, PluginConfigSummary, PluginSecretMetadata, PluginTaskReceiptSummary,
};
pub use audit::RecordPluginAudit;
pub use authority::{PluginConfigAuthority, PluginSecurityScope, PluginStateAuthority};
pub use developer_mode::{PLUGIN_DEVELOPER_MODE_SETTING_KEY, PluginDeveloperMode};
pub use grants::{PluginGrantRecord, PluginGrantStatus};
pub use idempotency::{PluginIdempotencyEnvelope, validate_idempotency_input};
pub use lifecycle::UninstalledPlugin;
pub use staged_operations::{
    AcceptStagedPluginOperation, AcceptedStagedPluginOperation, PluginAdminOperation,
    StagedPluginIntent, StagedPluginOpKind,
};
pub use state_types::*;
pub use trust_keys::{
    PinPluginTrustKey, PluginTrustKey, PluginTrustKeyStatus, PluginTrustKeySummary,
};
pub use ui_trust::{GrantPluginUiTrust, PluginUiTrustRecord};
pub use upgrade::{
    ApplyPluginPackageSwitch, PluginStatePolicy, PluginStateResetPreview, PluginStateResetResult,
    PluginUpgradeConfig,
};
mod command_authority;
mod commands;
pub use command_authority::{PluginCommandAuthority, PluginCommandTarget};
mod operation_payloads;
mod operation_rows;
mod operation_types;
mod operations;
mod reconciliation;
mod task_types;
mod tasks;
mod types;
pub use operation_types::*;
pub use task_types::*;
pub use types::*;
