use std::str::FromStr;

use crate::Db;
use crate::plugins::PluginError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OutboxDb {
    App,
    Subscribe,
    Library,
}

impl OutboxDb {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Subscribe => "subscribe",
            Self::Library => "library",
        }
    }
}

impl From<OutboxDb> for Db {
    fn from(value: OutboxDb) -> Self {
        match value {
            OutboxDb::App => Db::App,
            OutboxDb::Subscribe => Db::Subscribe,
            OutboxDb::Library => Db::Library,
        }
    }
}

impl TryFrom<Db> for OutboxDb {
    type Error = PluginError;

    fn try_from(value: Db) -> Result<Self, Self::Error> {
        match value {
            Db::App => Ok(Self::App),
            Db::Subscribe => Ok(Self::Subscribe),
            Db::Library => Ok(Self::Library),
            Db::Cache => Err(PluginError::Invalid(
                "cache database does not host an outbox",
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboxFanoutState {
    Pending,
    Seeded,
}

impl OutboxFanoutState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Seeded => "seeded",
        }
    }
}

impl FromStr for OutboxFanoutState {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "seeded" => Ok(Self::Seeded),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboxDeliveryStatus {
    Pending,
    Delivering,
    Delivered,
    DeadLetter,
}

impl OutboxDeliveryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Delivering => "delivering",
            Self::Delivered => "delivered",
            Self::DeadLetter => "dead_letter",
        }
    }
}

impl FromStr for OutboxDeliveryStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "delivering" => Ok(Self::Delivering),
            "delivered" => Ok(Self::Delivered),
            "dead_letter" => Ok(Self::DeadLetter),
            _ => Err(()),
        }
    }
}

/// Durable outbox event record. All timestamps (`occurred_at`, `created_at`) are Unix milliseconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxEventRecord {
    pub event_id: String,
    pub schema_version: i64,
    pub event_domain: String,
    pub event_kind: String,
    pub entity_type: String,
    pub entity_id: String,
    pub entity_sequence: i64,
    pub entity_revision: i64,
    pub cause_id: String,
    pub subject: String,
    pub site_id: Option<String>,
    pub subscribe_id: Option<String>,
    pub library_id: Option<String>,
    pub handles_json: String,
    pub payload_json: String,
    /// Unix milliseconds (`>= 0`).
    pub occurred_at: i64,
    pub fanout_state: OutboxFanoutState,
    /// Unix milliseconds (`>= 0`).
    pub created_at: i64,
}

/// Durable per-plugin delivery state. All timestamps are Unix milliseconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxDeliveryRecord {
    pub event_id: String,
    pub plugin_id: String,
    pub entity_type: String,
    pub entity_id: String,
    pub entity_sequence: i64,
    pub status: OutboxDeliveryStatus,
    pub attempt_count: i64,
    /// Unix milliseconds (`>= 0`).
    pub next_attempt_at: i64,
    /// Unix milliseconds (`>= 0`).
    pub claimed_at: Option<i64>,
    /// Unix milliseconds (`>= 0`).
    pub lease_until: Option<i64>,
    pub claim_nonce: Option<String>,
    pub last_error: Option<String>,
    pub component_state_json: String,
    /// Unix milliseconds (`>= 0`).
    pub delivered_at: Option<i64>,
    /// Unix milliseconds (`>= 0`).
    pub dead_lettered_at: Option<i64>,
    pub revision: i64,
    /// Unix milliseconds (`>= 0`).
    pub created_at: i64,
    /// Unix milliseconds (`>= 0`).
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxDeliveryDetail {
    pub event: OutboxEventRecord,
    pub delivery: OutboxDeliveryRecord,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DueOutboxDelivery {
    pub event: OutboxEventRecord,
    pub delivery: OutboxDeliveryRecord,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimedOutboxDelivery {
    pub event: OutboxEventRecord,
    pub delivery: OutboxDeliveryRecord,
    pub claim_nonce: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxDiagnostics {
    pub db: OutboxDb,
    pub outbox_total: u64,
    pub pending_fanout_count: u64,
    pub pending_count: u64,
    pub due_count: u64,
    pub delivering_count: u64,
    pub delivered_count: u64,
    pub dead_letter_count: u64,
    /// Unix milliseconds (`>= 0`).
    pub oldest_pending_created_at: Option<i64>,
    /// Lag in milliseconds (`now_ms - oldest_pending_created_at`).
    pub lag_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmitOutboxEventInput {
    pub event_id: String,
    pub schema_version: i64,
    pub event_domain: String,
    pub event_kind: String,
    pub entity_type: String,
    pub entity_id: String,
    pub entity_sequence: Option<i64>,
    pub entity_revision: i64,
    pub cause_id: String,
    pub subject: String,
    pub site_id: Option<String>,
    pub subscribe_id: Option<String>,
    pub library_id: Option<String>,
    pub handles_json: String,
    pub payload_json: String,
    /// Unix milliseconds (`>= 0`).
    pub occurred_at: i64,
    /// Unix milliseconds (`>= 0`).
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeedDeliveryInput {
    pub event_id: String,
    pub plugin_id: String,
    /// Unix milliseconds (`>= 0`).
    pub next_attempt_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeedSubscriberDeliveriesInput {
    pub event_id: String,
    pub plugin_ids: Vec<String>,
    /// Unix milliseconds (`>= 0`).
    pub next_attempt_at: i64,
    /// Unix milliseconds (`>= 0`).
    pub now_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimDueDeliveriesInput {
    pub plugin_id: Option<String>,
    /// Unix milliseconds (`>= 0`).
    pub now_ms: i64,
    pub lease_duration_ms: i64,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkDeliveredInput {
    pub event_id: String,
    pub plugin_id: String,
    pub claim_nonce: String,
    /// Unix milliseconds (`>= 0`).
    pub now_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleRetryInput {
    pub event_id: String,
    pub plugin_id: String,
    pub claim_nonce: String,
    /// Unix milliseconds (`>= 0`).
    pub next_attempt_at: i64,
    pub error: String,
    /// Unix milliseconds (`>= 0`).
    pub now_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkDeadLetterInput {
    pub event_id: String,
    pub plugin_id: String,
    pub claim_nonce: String,
    pub error: String,
    /// Unix milliseconds (`>= 0`).
    pub now_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutboxEventFilter {
    pub event_domain: Option<String>,
    pub event_kind: Option<String>,
    pub entity_type: Option<String>,
    pub entity_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxEventCursor {
    pub created_at: i64,
    pub event_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryDueCursor {
    pub next_attempt_at: i64,
    pub event_id: String,
    pub plugin_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeadLetterCursor {
    pub dead_lettered_at: i64,
    pub event_id: String,
    pub plugin_id: String,
}
