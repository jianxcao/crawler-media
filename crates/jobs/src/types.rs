use domain::{JobDefId, JobId};

use crate::kinds::{JobKind, Schedule};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl std::str::FromStr for JobStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(other.to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub id: JobId,
    pub def_id: Option<JobDefId>,
    pub kind: JobKind,
    pub payload: String,
    pub status: JobStatus,
    pub attempt: u32,
    pub run_after: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub error: Option<String>,
    pub progress: Option<String>,
    pub concurrency_key: Option<String>,
    pub timeout_secs: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobDef {
    pub id: JobDefId,
    pub kind: JobKind,
    pub name: String,
    pub enabled: bool,
    pub schedule: Option<Schedule>,
    pub payload: String,
    pub timeout_secs: Option<u32>,
    pub concurrency_key: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("unknown Job {0}")]
    UnknownJob(JobId),
    #[error("invalid job kind: {0}")]
    Kind(String),
    #[error("invalid job status: {0}")]
    Status(String),
    #[error("invalid schedule: {0}")]
    Schedule(String),
}
