use std::fmt;
use std::str::FromStr;

use domain::{DownloaderId, SiteId, SubscribeId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadOperationState {
    Intent,
    Reserved,
    Submitting,
    Submitted,
    Rejected,
    Deferred,
    Failed,
    Unknown,
}

impl DownloadOperationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Reserved => "reserved",
            Self::Submitting => "submitting",
            Self::Submitted => "submitted",
            Self::Rejected => "rejected",
            Self::Deferred => "deferred",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

impl FromStr for DownloadOperationState {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "intent" => Ok(Self::Intent),
            "reserved" => Ok(Self::Reserved),
            "submitting" => Ok(Self::Submitting),
            "submitted" => Ok(Self::Submitted),
            "rejected" => Ok(Self::Rejected),
            "deferred" => Ok(Self::Deferred),
            "failed" => Ok(Self::Failed),
            "unknown" => Ok(Self::Unknown),
            _ => Err("unknown download operation state"),
        }
    }
}

impl fmt::Display for DownloadOperationState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadOwnershipState {
    Owned,
    Foreign,
    Unproven,
    Broken,
    Deleted,
}

impl DownloadOwnershipState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owned => "owned",
            Self::Foreign => "foreign",
            Self::Unproven => "unproven",
            Self::Broken => "broken",
            Self::Deleted => "deleted",
        }
    }
}

impl FromStr for DownloadOwnershipState {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "owned" => Ok(Self::Owned),
            "foreign" => Ok(Self::Foreign),
            "unproven" => Ok(Self::Unproven),
            "broken" => Ok(Self::Broken),
            "deleted" => Ok(Self::Deleted),
            _ => Err("unknown download ownership state"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadProvenanceKind {
    CreatedFresh,
    ReconciledOperation,
    ExistingOwned,
    ExistingForeign,
    BrokenExisting,
    Unproven,
}

impl DownloadProvenanceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CreatedFresh => "created_fresh",
            Self::ReconciledOperation => "reconciled_operation",
            Self::ExistingOwned => "existing_owned",
            Self::ExistingForeign => "existing_foreign",
            Self::BrokenExisting => "broken_existing",
            Self::Unproven => "unproven",
        }
    }
}

impl FromStr for DownloadProvenanceKind {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "created_fresh" => Ok(Self::CreatedFresh),
            "reconciled_operation" => Ok(Self::ReconciledOperation),
            "existing_owned" => Ok(Self::ExistingOwned),
            "existing_foreign" => Ok(Self::ExistingForeign),
            "broken_existing" => Ok(Self::BrokenExisting),
            "unproven" => Ok(Self::Unproven),
            _ => Err("unknown download provenance kind"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoostReservationStage {
    Provisional,
    Final,
    UnknownHeld,
    Committed,
    Released,
}

impl BoostReservationStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Provisional => "provisional",
            Self::Final => "final",
            Self::UnknownHeld => "unknown_held",
            Self::Committed => "committed",
            Self::Released => "released",
        }
    }
}

impl FromStr for BoostReservationStage {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "provisional" => Ok(Self::Provisional),
            "final" => Ok(Self::Final),
            "unknown_held" => Ok(Self::UnknownHeld),
            "committed" => Ok(Self::Committed),
            "released" => Ok(Self::Released),
            _ => Err("unknown boost reservation stage"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoostTaskState {
    Downloading,
    Seeding,
    Paused,
    RetainedOnly,
    CleanupPlanned,
    Cleaned,
}

impl BoostTaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Downloading => "downloading",
            Self::Seeding => "seeding",
            Self::Paused => "paused",
            Self::RetainedOnly => "retained_only",
            Self::CleanupPlanned => "cleanup_planned",
            Self::Cleaned => "cleaned",
        }
    }
}

impl FromStr for BoostTaskState {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "downloading" => Ok(Self::Downloading),
            "seeding" => Ok(Self::Seeding),
            "paused" => Ok(Self::Paused),
            "retained_only" => Ok(Self::RetainedOnly),
            "cleanup_planned" => Ok(Self::CleanupPlanned),
            "cleaned" => Ok(Self::Cleaned),
            _ => Err("unknown boost task state"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoostProtectionState {
    Unknown,
    Protected,
    Eligible,
    Blocked,
}

impl BoostProtectionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Protected => "protected",
            Self::Eligible => "eligible",
            Self::Blocked => "blocked",
        }
    }
}

impl FromStr for BoostProtectionState {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "unknown" => Ok(Self::Unknown),
            "protected" => Ok(Self::Protected),
            "eligible" => Ok(Self::Eligible),
            "blocked" => Ok(Self::Blocked),
            _ => Err("unknown boost protection state"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscribeDeferState {
    Deferred,
    Rejected,
    Invalidated,
    Resolved,
}

impl SubscribeDeferState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deferred => "deferred",
            Self::Rejected => "rejected",
            Self::Invalidated => "invalidated",
            Self::Resolved => "resolved",
        }
    }
}

impl FromStr for SubscribeDeferState {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "deferred" => Ok(Self::Deferred),
            "rejected" => Ok(Self::Rejected),
            "invalidated" => Ok(Self::Invalidated),
            "resolved" => Ok(Self::Resolved),
            _ => Err("unknown subscribe defer state"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadOperationRecord {
    pub operation_id: String,
    pub idempotency_key: String,
    pub source_kind: String,
    pub subscribe_id: Option<SubscribeId>,
    pub plugin_id: Option<String>,
    pub subject: String,
    pub cause_id: String,
    pub site_id: Option<SiteId>,
    pub downloader_id: Option<DownloaderId>,
    pub endpoint_fingerprint: Option<String>,
    pub enclosure: String,
    pub ownership_tag: String,
    pub info_hash: Option<String>,
    pub bytes_digest: Option<String>,
    pub verified_bytes: Option<i64>,
    pub request_digest: String,
    pub state: DownloadOperationState,
    pub outcome_code: Option<String>,
    pub error_message: Option<String>,
    pub revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub submitted_at: Option<i64>,
    pub reconciled_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptDownloadOperationInput {
    pub operation_id: String,
    pub idempotency_key: String,
    pub source_kind: String,
    pub subscribe_id: Option<SubscribeId>,
    pub plugin_id: Option<String>,
    pub subject: String,
    pub cause_id: String,
    pub site_id: Option<SiteId>,
    pub downloader_id: Option<DownloaderId>,
    pub endpoint_fingerprint: Option<String>,
    pub enclosure: String,
    pub ownership_tag: String,
    pub info_hash: Option<String>,
    pub bytes_digest: Option<String>,
    pub verified_bytes: Option<i64>,
    pub request_digest: String,
    pub now_utc: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedDownloadOperation {
    pub operation: DownloadOperationRecord,
    pub replayed: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DownloadOperationFilter {
    pub subscribe_id: Option<SubscribeId>,
    pub plugin_id: Option<String>,
    pub site_id: Option<SiteId>,
    pub downloader_id: Option<DownloaderId>,
    pub state: Option<DownloadOperationState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitionDownloadOperationInput {
    pub operation_id: String,
    pub expected_revision: i64,
    pub next_state: DownloadOperationState,
    pub downloader_id: Option<DownloaderId>,
    pub endpoint_fingerprint: Option<String>,
    pub info_hash: Option<String>,
    pub bytes_digest: Option<String>,
    pub verified_bytes: Option<i64>,
    pub outcome_code: Option<String>,
    pub error_message: Option<String>,
    pub submitted_at: Option<i64>,
    pub now_utc: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadOwnershipRecord {
    pub downloader_id: DownloaderId,
    pub ownership_tag: String,
    pub info_hash: Option<String>,
    pub operation_id: String,
    pub subscribe_id: Option<SubscribeId>,
    pub plugin_id: Option<String>,
    pub subject: String,
    pub site_id: Option<SiteId>,
    pub enclosure: String,
    pub ownership_state: DownloadOwnershipState,
    pub provenance_kind: DownloadProvenanceKind,
    pub verified_bytes: Option<i64>,
    pub bytes_digest: Option<String>,
    pub revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordDownloadOwnershipInput {
    pub downloader_id: DownloaderId,
    pub ownership_tag: String,
    pub info_hash: Option<String>,
    pub operation_id: String,
    pub subscribe_id: Option<SubscribeId>,
    pub plugin_id: Option<String>,
    pub subject: String,
    pub site_id: Option<SiteId>,
    pub enclosure: String,
    pub ownership_state: DownloadOwnershipState,
    pub provenance_kind: DownloadProvenanceKind,
    pub verified_bytes: Option<i64>,
    pub bytes_digest: Option<String>,
    pub now_utc: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconcileDownloadOperationInput {
    pub operation_id: String,
    pub expected_revision: i64,
    pub resolved_state: DownloadOperationState,
    pub info_hash: Option<String>,
    pub outcome_code: Option<String>,
    pub error_message: Option<String>,
    pub ownership: Option<RecordDownloadOwnershipInput>,
    pub boost_task_fact: Option<UpsertBoostTaskFactInput>,
    pub now_utc: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoostReservationRecord {
    pub reservation_id: String,
    pub operation_id: String,
    pub quota_key: String,
    pub plugin_id: String,
    pub site_id: Option<SiteId>,
    pub downloader_id: DownloaderId,
    pub info_hash: Option<String>,
    pub bytes_digest: Option<String>,
    pub reserved_bytes: i64,
    pub stage: BoostReservationStage,
    pub expires_at: Option<i64>,
    pub revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReserveBoostProvisionalInput {
    pub reservation_id: String,
    pub operation_id: String,
    pub quota_key: String,
    pub plugin_id: String,
    pub site_id: Option<SiteId>,
    pub downloader_id: DownloaderId,
    pub info_hash: Option<String>,
    pub estimated_bytes: i64,
    pub expires_at: Option<i64>,
    pub now_utc: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitBoostFinalInput {
    pub operation_id: String,
    pub expected_reservation_revision: i64,
    pub info_hash: String,
    pub bytes_digest: String,
    pub verified_bytes: i64,
    pub budget_bytes: i64,
    pub max_add_per_hour: Option<i64>,
    pub mark_operation_submitting: bool,
    pub now_utc: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoostTaskFactRecord {
    pub task_fact_id: String,
    pub plugin_id: String,
    pub quota_key: String,
    pub downloader_id: DownloaderId,
    pub ownership_tag: String,
    pub info_hash: String,
    pub site_id: Option<SiteId>,
    pub state: BoostTaskState,
    pub occupied_bytes: i64,
    pub retained_bytes: i64,
    pub hr_active: Option<bool>,
    pub hr_until: Option<i64>,
    pub hold_until: Option<i64>,
    pub free_until: Option<i64>,
    pub protection_state: BoostProtectionState,
    pub protection_reason: Option<String>,
    pub cleanup_plan_handle: Option<String>,
    pub cleanup_plan_expires_at: Option<i64>,
    pub cleanup_plan_json: Option<String>,
    pub observed_at: i64,
    pub revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpsertBoostTaskFactInput {
    pub task_fact_id: String,
    pub plugin_id: String,
    pub quota_key: String,
    pub downloader_id: DownloaderId,
    pub ownership_tag: String,
    pub info_hash: String,
    pub site_id: Option<SiteId>,
    pub state: BoostTaskState,
    pub occupied_bytes: i64,
    pub retained_bytes: i64,
    pub hr_active: Option<bool>,
    pub hr_until: Option<i64>,
    pub hold_until: Option<i64>,
    pub free_until: Option<i64>,
    pub protection_state: BoostProtectionState,
    pub protection_reason: Option<String>,
    pub observed_at: i64,
    pub expected_revision: Option<i64>,
    pub now_utc: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizeBoostTaskInput {
    pub operation_id: String,
    pub expected_reservation_revision: i64,
    pub task_fact: UpsertBoostTaskFactInput,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrepareBoostCleanupPlanInput {
    pub task_fact_id: String,
    pub expected_revision: i64,
    pub cleanup_plan_handle: String,
    pub cleanup_plan_json: String,
    pub ttl_secs: i64,
    pub now_utc: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoostSnapshotFacts {
    pub used_bytes: i64,
    pub reserved_bytes: i64,
    pub retained_file_bytes: i64,
    pub reservations: Vec<BoostReservationRecord>,
    pub tasks: Vec<BoostTaskFactRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscribePolicyBinding {
    pub subscribe_id: SubscribeId,
    pub jobs_sync_generation: String,
    pub policy_revision: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscribeDeferredCandidateRecord {
    pub defer_id: String,
    pub subscribe_id: SubscribeId,
    pub subscribe_generation: String,
    pub policy_revision: i64,
    pub enclosure: String,
    pub info_hash: Option<String>,
    pub plugin_id: Option<String>,
    pub hook_id: String,
    pub reason: String,
    pub attempt_count: i64,
    pub first_deferred_at: i64,
    pub next_retry_at: i64,
    pub max_deadline_at: i64,
    pub state: SubscribeDeferState,
    pub revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordSubscribeDeferInput {
    pub defer_id: String,
    pub subscribe_id: SubscribeId,
    pub subscribe_generation: String,
    pub policy_revision: i64,
    pub enclosure: String,
    pub info_hash: Option<String>,
    pub plugin_id: Option<String>,
    pub hook_id: String,
    pub reason: String,
    pub next_retry_at: i64,
    pub now_utc: i64,
}
