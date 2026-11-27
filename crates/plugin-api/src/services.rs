#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuestRole {
    Filter,
    Gate,
    Notify,
    Task,
    Endpoint,
    Indexer,
    Maintenance,
    Lifecycle,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PluginTrust {
    #[default]
    Untrusted,
    BuiltIn,
    AdminTrusted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HealthState {
    Healthy,
    Degraded(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigView {
    pub revision: u64,
    pub validated_json: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateEntry {
    pub key: String,
    pub value: Vec<u8>,
    pub etag: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryPage {
    pub items: Vec<StateEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageRequest {
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationState {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationView {
    pub id: String,
    pub state: OperationState,
    pub revision: u64,
    pub next_run_at: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationPage {
    pub items: Vec<OperationView>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaintenanceAction {
    VerifySession,
    CheckIn,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnedRevision {
    pub handle: String,
    pub revision: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DownloadOptions {
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
pub enum TaskControl {
    Pause,
    Resume,
    Tags(Vec<String>),
    Category(String),
    Limits {
        upload: Option<u64>,
        download: Option<u64>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoostDisk {
    pub available_bytes: Option<u64>,
    pub observed_at: u64,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoostReservationView {
    pub handle: String,
    pub operation_id: String,
    pub quota_key: String,
    pub downloader_id: String,
    pub site_id: Option<String>,
    pub info_hash: Option<String>,
    pub reserved_bytes: u64,
    pub stage: String,
    pub expires_at: Option<u64>,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoostTaskView {
    pub handle: String,
    pub quota_key: String,
    pub downloader_id: String,
    pub site_id: Option<String>,
    pub info_hash: String,
    pub state: String,
    pub title: String,
    pub progress_permille: u32,
    pub downloaded_bytes: u64,
    pub upload_speed: u64,
    pub download_speed: u64,
    pub seeding_secs: u64,
    pub added_at: Option<u64>,
    pub completed_at: Option<u64>,
    pub last_activity_at: Option<u64>,
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub task_bytes: u64,
    pub retained_bytes: u64,
    pub uploaded_bytes: u64,
    pub hr_protected: Option<bool>,
    pub hold_until: Option<u64>,
    pub free_until: Option<u64>,
    pub protection_state: String,
    pub transfer_active: bool,
    pub library_referenced: bool,
    pub observed_at: u64,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoostSnapshotView {
    pub revision: u64,
    pub used_bytes: u64,
    pub reserved_bytes: u64,
    pub retained_file_bytes: u64,
    pub reservations: Vec<BoostReservationView>,
    pub tasks: Vec<BoostTaskView>,
    pub disk: BoostDisk,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandAction {
    SiteMaintenance {
        site_handle: String,
        action: MaintenanceAction,
    },
    ScheduleSet {
        task_id: String,
        interval_secs: u32,
        enabled: bool,
        expected_revision: u64,
    },
    ScheduleDisable {
        handle: String,
        revision: u64,
    },
    DownloaderAdd {
        downloader_id: String,
        download_ref: String,
        options: DownloadOptions,
    },
    DownloaderControl {
        target: OwnedRevision,
        control: TaskControl,
    },
    DownloaderDeleteTask {
        downloader_id: String,
        revision: u64,
        task_handle: String,
    },
    DownloaderPrepareCleanup {
        targets: Vec<OwnedRevision>,
        reason: String,
    },
    DownloaderDeleteFiles {
        target: OwnedRevision,
        plan_handle: String,
    },
    Other {
        kind: String,
        summary: String,
    },
}

impl CommandAction {
    pub fn kind_name(&self) -> &str {
        match self {
            Self::SiteMaintenance { action, .. } => match action {
                MaintenanceAction::VerifySession => "site.verify-session",
                MaintenanceAction::CheckIn => "site.check-in",
            },
            Self::ScheduleSet { .. } => "schedule.set",
            Self::ScheduleDisable { .. } => "schedule.disable",
            Self::DownloaderAdd { .. } => "downloader.add",
            Self::DownloaderControl { .. } => "downloader.control",
            Self::DownloaderDeleteTask { .. } => "downloader.delete-task",
            Self::DownloaderPrepareCleanup { .. } => "downloader.prepare-cleanup",
            Self::DownloaderDeleteFiles { .. } => "downloader.delete-files",
            Self::Other { kind, .. } => kind.as_str(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandRequest {
    pub action: CommandAction,
    pub idempotency_key: String,
    pub expected_revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskInput {
    pub task_id: String,
    pub validated_input: String,
    pub job_id: String,
    pub attempt: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskOutcome {
    pub partial: bool,
    pub progress: u32,
    pub operation_ids: Vec<String>,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointRequest {
    pub method: String,
    pub path: String,
    pub query: Vec<Header>,
    pub body: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointResponse {
    pub status: u16,
    pub headers: Vec<Header>,
    pub body: Vec<u8>,
}
