use domain::{FilePresence, FileVersion, LedgerId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresenceMode {
    Verify,
    Prune,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresenceObservation {
    pub file_id: LedgerId,
    pub observed_version: FileVersion,
    pub observed_path: std::path::PathBuf,
    pub presence: FilePresence,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PresenceReport {
    pub missing: usize,
    pub restored: usize,
    pub deleted: usize,
    pub skipped: usize,
}
