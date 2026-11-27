use super::{PluginGrantScope, PluginScopedBytes};
use serde::{Deserialize, Serialize};

/// Constructed by authenticated host services, never decoded from guest input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginSubject {
    User(domain::UserId),
    EndpointKey {
        key_id: uuid::Uuid,
        issuer_user_id: domain::UserId,
    },
    Task {
        plugin_id: String,
        task_id: String,
    },
}

/// Method/path normalization and body canonicalization are the host's responsibility.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalPluginRequest {
    pub method: String,
    pub path: String,
    pub body_digest: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginCauseHop {
    pub plugin_id: String,
    pub kind: String,
    pub target_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginCommandCause {
    pub cause_id: String,
    pub depth: u8,
    pub visited: Vec<PluginCauseHop>,
}

impl PluginCommandCause {
    pub fn root(cause_id: impl Into<String>) -> Self {
        Self {
            cause_id: cause_id.into(),
            depth: 0,
            visited: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AcceptPluginOperation {
    pub plugin_id: Option<String>,
    pub subject: PluginSubject,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
    pub kind: String,
    pub expected_revision: Option<i64>,
    pub expected_generation: Option<i64>,
    pub cause: PluginCommandCause,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginClaimedCommand {
    pub operation: PluginOperation,
    pub is_runtime: bool,
    pub subject: PluginSubject,
    pub canonical_method: String,
    pub targets: Vec<PluginGrantScope>,
    pub cause: PluginCommandCause,
    pub payload: Option<PluginScopedBytes>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginOperationState {
    Queued,
    Running,
    Waiting,
    Blocked,
    Succeeded,
    Rejected,
    Deferred,
    Failed,
    Cancelled,
    Unknown,
}

impl PluginOperationState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Rejected | Self::Failed | Self::Cancelled
        )
    }
}

/// Redacted projection: command bytes and scoped results are deliberately absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginOperation {
    pub operation_id: uuid::Uuid,
    pub plugin_id: Option<String>,
    pub kind: String,
    pub revision: i64,
    pub generation: Option<i64>,
    pub state: PluginOperationState,
    pub phase: String,
    pub next_run_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    pub effect: PluginEffectObservation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedPluginOperation {
    pub operation: PluginOperation,
    pub replayed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginCommandCursor {
    pub next_run_at: i64,
    pub operation_id: uuid::Uuid,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginPendingCommand {
    pub operation: PluginOperation,
    pub command_revision: i64,
    pub attempt: i64,
    pub targets: Vec<PluginGrantScope>,
    pub cursor: PluginCommandCursor,
}

/// Opaque ownership of one claim. Never deserialize this from a request or guest.
#[derive(Clone, Debug)]
pub struct PluginCommandClaim {
    pub(super) operation_id: uuid::Uuid,
    pub(super) revision: i64,
    pub(super) command_revision: i64,
    pub(super) attempt: i64,
    pub(super) generation: Option<i64>,
    pub(super) nonce: uuid::Uuid,
}
impl PluginCommandClaim {
    pub fn operation_id(&self) -> uuid::Uuid {
        self.operation_id
    }
    pub fn revision(&self) -> i64 {
        self.revision
    }
    pub fn attempt(&self) -> i64 {
        self.attempt
    }
    pub fn generation(&self) -> Option<i64> {
        self.generation
    }
}

/// Host phase codes only; arbitrary guest output belongs in the scoped result.
pub struct PluginCommandOutcome {
    pub state: PluginOperationState,
    pub phase: String,
    pub next_run_at: Option<i64>,
    pub result: Option<Vec<u8>>,
    pub effect: PluginEffectObservation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginEffectObservation {
    NoEffect,
    Confirmed,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginReconciliationOutcome {
    Succeeded,
    Failed,
    ConfirmedNoEffect,
}
/// Explicit host reconciliation, distinct from retrying a guest invocation.
pub struct PluginReconciliationObservation {
    pub operation_id: uuid::Uuid,
    pub revision: i64,
    pub generation: Option<i64>,
    pub attempt: i64,
    pub outcome: PluginReconciliationOutcome,
}
