use crate::{Candidate, SearchQuery};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HookContext {
    pub invocation_id: String,
    pub cause_id: String,
    pub plugin_id: String,
    pub generation: u64,
    pub config_revision: u64,
    pub subject: String,
    pub site_id: Option<String>,
    pub subscribe_id: Option<String>,
    pub library_id: Option<String>,
    pub deadline_ms: u64,
    pub cancelled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchBefore {
    pub query: SearchQuery,
    pub site_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchSiteAfter {
    pub site_id: String,
    pub query: SearchQuery,
    pub candidates: Vec<Candidate>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterAfter {
    pub candidates: Vec<Candidate>,
    pub admitted_handles: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscribeScore {
    pub subscribe_id: String,
    pub media_id: String,
    pub candidates: Vec<Candidate>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscribeSelect {
    pub subscribe_id: String,
    pub media_id: String,
    pub candidates: Vec<Candidate>,
    pub admitted_handles: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadPlanView {
    pub downloader_id: String,
    pub relative_directory: Option<String>,
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub paused: bool,
    pub upload_limit: Option<u64>,
    pub download_limit: Option<u64>,
    pub sequential: bool,
    pub first_last_piece: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadBefore {
    pub candidate: Candidate,
    pub purpose: String,
    pub plan: DownloadPlanView,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TransferRename {
    pub media_id: String,
    pub library_id: String,
    pub source_path: String,
    pub proposed_relative_path: String,
    pub is_movie: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilterPayload {
    SearchBefore(SearchBefore),
    SearchSiteAfter(SearchSiteAfter),
    SearchAfter(crate::SearchAfter),
    FilterAfter(FilterAfter),
    SubscribeScore(SubscribeScore),
    SubscribeSelect(SubscribeSelect),
    DownloadBefore(Box<DownloadBefore>),
    TransferRename(TransferRename),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterInput {
    pub schema_version: u32,
    pub context: HookContext,
    pub payload: FilterPayload,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RescorePatch {
    pub handle: String,
    pub delta: i32,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchQueryPatch {
    pub keyword: Option<String>,
    pub site_ids: Option<Vec<String>>,
    pub categories: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DownloadPlanPatch {
    pub downloader_id: Option<String>,
    pub relative_directory: Option<String>,
    pub category: Option<String>,
    pub add_tags: Vec<String>,
    pub remove_tags: Vec<String>,
    pub paused: Option<bool>,
    pub upload_limit: Option<u64>,
    pub download_limit: Option<u64>,
    pub sequential: Option<bool>,
    pub first_last_piece: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Patch {
    Drop(String),
    SetOrder(Vec<String>),
    Rescore(RescorePatch),
    SearchQuery(SearchQueryPatch),
    SelectHandles(Vec<String>),
    DownloadPlan(DownloadPlanPatch),
    RenamePath(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GatePayload {
    None,
    SearchBefore(SearchBefore),
    SubscribeSelect(SubscribeSelect),
    DownloadBefore(Box<DownloadBefore>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateInput {
    pub schema_version: u32,
    pub context: HookContext,
    pub hook_id: String,
    pub handles: Vec<String>,
    pub revision: u64,
    pub payload: GatePayload,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateDecision {
    Allow,
    Abstain,
    Reject(String),
    Defer { until_ms: u64, reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DownloadAfterEvent {
    pub operation_id: String,
    pub downloader_id: Option<String>,
    pub info_hash: Option<String>,
    pub outcome: String,
    pub verified_total_bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TransferCompletedEvent {
    pub media_id: String,
    pub library_id: String,
    pub relative_path: String,
    pub file_count: u32,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SiteCheckinAfterEvent {
    pub site_id: String,
    pub action: String,
    pub state: String,
    pub window_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum EventPayload {
    None,
    DownloadAfter(DownloadAfterEvent),
    TransferCompleted(TransferCompletedEvent),
    SiteCheckinAfter(SiteCheckinAfterEvent),
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Event {
    pub schema_version: u32,
    pub context: HookContext,
    pub id: String,
    pub kind: String,
    pub occurred_at: u64,
    pub handles: Vec<String>,
    pub revision: u64,
    pub payload: EventPayload,
}
