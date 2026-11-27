use crate::Header;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteTarget {
    pub path: String,
    pub query: Vec<Header>,
    pub auth_slot: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HttpTarget {
    PublicUrl(String),
    SiteRelative(SiteTarget),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub target: HttpTarget,
    pub headers: Vec<Header>,
    pub body: Option<Vec<u8>>,
    pub max_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<Header>,
    pub body: Vec<u8>,
    pub truncated: bool,
    pub receipt: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteView {
    pub handle: String,
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub profile_ref: String,
    pub capabilities: Vec<String>,
    pub maintenance_policy: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SitePage {
    pub items: Vec<SiteView>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatsView {
    pub uploaded: Option<u64>,
    pub downloaded: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Category {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Descriptor {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub capabilities: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchQuery {
    pub keyword: String,
    pub page: u32,
    pub categories: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TorrentRaw {
    pub provider_local_id: String,
    pub title: String,
    pub size: u64,
    pub download_ref: String,
    pub seeders: Option<u32>,
    pub leechers: Option<u32>,
    pub published_at: Option<u64>,
    pub category: Option<String>,
    pub free: Option<bool>,
    pub hr: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TorrentBatch {
    pub items: Vec<TorrentRaw>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaintenanceState {
    Completed,
    AlreadyCompleted,
    AuthExpired,
    Challenge,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaintenanceOutcome {
    pub state: MaintenanceState,
    pub evidence: String,
    pub session_capture: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserPurpose {
    Search,
    Maintenance,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserElement {
    pub handle: String,
    pub visible_text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowserFillValue {
    Text(String),
    CredentialSlot(String),
}
