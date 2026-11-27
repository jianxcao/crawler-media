use domain::SiteId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SiteMaintenanceAction {
    VerifySession,
    CheckIn,
}

impl SiteMaintenanceAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VerifySession => "verify-session",
            Self::CheckIn => "check-in",
        }
    }
}

impl std::str::FromStr for SiteMaintenanceAction {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "verify-session" => Ok(Self::VerifySession),
            "check-in" => Ok(Self::CheckIn),
            _ => Err("invalid site maintenance action"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SiteMaintenanceState {
    Pending,
    Completed,
    Unknown,
    Failed,
}

impl SiteMaintenanceState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::Unknown => "unknown",
            Self::Failed => "failed",
        }
    }
}

impl std::str::FromStr for SiteMaintenanceState {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "completed" => Ok(Self::Completed),
            "unknown" => Ok(Self::Unknown),
            "failed" => Ok(Self::Failed),
            _ => Err("invalid site maintenance state"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteMaintenanceReceipt {
    pub receipt_id: String,
    pub site_id: SiteId,
    pub action: SiteMaintenanceAction,
    pub window_key: String,
    pub timezone: String,
    pub window_start_utc: i64,
    pub window_end_utc: i64,
    pub plugin_id: String,
    pub provider_id: String,
    pub generation: i64,
    pub credential_epoch: String,
    pub state: SiteMaintenanceState,
    pub outcome: Option<String>,
    pub evidence: Option<String>,
    pub revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimSiteMaintenanceReceipt {
    pub site_id: SiteId,
    pub action: SiteMaintenanceAction,
    pub window_key: String,
    pub timezone: String,
    pub window_start_utc: i64,
    pub window_end_utc: i64,
    pub now_utc: i64,
    pub plugin_id: String,
    pub provider_id: String,
    pub generation: i64,
    pub credential_epoch: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SiteMaintenanceClaimResult {
    Claimed(SiteMaintenanceReceipt),
    AlreadyCompleted(SiteMaintenanceReceipt),
    BlockedUnknown(SiteMaintenanceReceipt),
    InFlight(SiteMaintenanceReceipt),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizeSiteMaintenanceReceipt {
    pub receipt_id: String,
    pub expected_revision: i64,
    pub state: SiteMaintenanceState,
    pub outcome: Option<String>,
    pub evidence: Option<String>,
    pub now_utc: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolveSiteMaintenanceReceipt {
    pub receipt_id: String,
    pub expected_revision: i64,
    pub resolved_state: SiteMaintenanceState,
    pub outcome: Option<String>,
    pub evidence: Option<String>,
    pub now_utc: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SiteMaintenanceReceiptFilter {
    pub site_id: Option<SiteId>,
    pub action: Option<SiteMaintenanceAction>,
    pub state: Option<SiteMaintenanceState>,
}
