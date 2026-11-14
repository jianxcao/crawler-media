use serde::{Deserialize, Serialize};

use super::ChapterFrameTask;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChapterFrameState {
    Queued,
    Running,
    Completed,
    Partial,
    Failed,
    Cancelled,
    Superseded,
}

impl ChapterFrameState {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Superseded => "superseded",
        }
    }

    pub(super) fn fenced(self) -> bool {
        matches!(self, Self::Cancelled | Self::Superseded)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChapterFrameTargetResult {
    pub index: usize,
    pub ready: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChapterFrameTaskStatus {
    pub task_id: String,
    pub generation: String,
    pub attempt: u64,
    pub state: ChapterFrameState,
    pub total: usize,
    pub ready: usize,
    pub failed: usize,
    pub targets: Vec<ChapterFrameTargetResult>,
}

/// Immutable physical output identity, allocated durably before rendering.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChapterFrameArtifact {
    pub attempt: u64,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChapterFrameTargetSnapshot {
    pub index: usize,
    pub ready: bool,
    pub error: Option<String>,
    pub published: Option<ChapterFrameArtifact>,
    pub candidate: Option<ChapterFrameArtifact>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChapterFrameTaskSnapshot {
    pub task: ChapterFrameTask,
    pub status: ChapterFrameTaskStatus,
    pub targets: Vec<ChapterFrameTargetSnapshot>,
}

impl ChapterFrameTaskSnapshot {
    pub(super) fn refresh_status(&mut self) {
        self.status.total = self.targets.len();
        self.status.ready = self.targets.iter().filter(|target| target.ready).count();
        self.status.failed = self
            .targets
            .iter()
            .filter(|target| target.error.is_some())
            .count();
        self.status.targets = self
            .targets
            .iter()
            .map(|target| ChapterFrameTargetResult {
                index: target.index,
                ready: target.ready,
                error: target.error.clone(),
            })
            .collect();
    }

    pub(super) fn terminal_state(&self) -> ChapterFrameState {
        let ready = self.targets.iter().filter(|target| target.ready).count();
        if ready == self.targets.len() {
            ChapterFrameState::Completed
        } else if ready == 0 {
            ChapterFrameState::Failed
        } else {
            ChapterFrameState::Partial
        }
    }
}

#[derive(Clone, Debug)]
pub struct ChapterFrameClaim {
    pub task: ChapterFrameTask,
    pub generation: String,
    pub attempt: u64,
    pub targets: Vec<ChapterFrameTargetSnapshot>,
}

#[derive(Clone, Debug)]
pub struct ChapterFrameReconcile {
    pub applied: bool,
    pub status: ChapterFrameTaskStatus,
}
