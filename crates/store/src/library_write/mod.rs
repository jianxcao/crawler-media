use domain::{FileVersion, LedgerId, LibraryFile, LibraryId, UnitKey};
use std::path::PathBuf;

pub mod artwork;
pub mod delete;
pub mod presence;
pub mod probe;
pub mod publish;
pub mod reassign;
pub mod reduce;
pub mod reduce_incremental;
pub mod rename;
pub mod rename_disk;
mod rename_durable;
mod rename_phases;
mod rename_receipt;
mod rename_record;
pub mod rename_sidecars;
pub mod source;
pub mod transfer;

pub use artwork::*;
pub use presence::*;
pub use publish::*;
pub use reassign::*;
pub use reduce::*;
pub use rename_durable::{FileRenameRequest, RenameSidecar, RenameSourceEvidence, SidecarAction};
pub use rename_phases::{
    AppliedRenameSidecars, DecidedRenameSidecars, ObservedRenameSidecars, RenameSidecarPlan,
};
pub use rename_receipt::{RenameDiskReceipt, execute_rename_disk, execute_rename_disk_copying};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteScope {
    pub library_id: LibraryId,
    pub config_revision: i64,
}

pub struct FilePublication<'a> {
    pub file: &'a LibraryFile,
    pub units: &'a [UnitKey],
    pub expected_version: Option<&'a FileVersion>,
    /// 本次发布要落库的 NFO 观察：一个视频文件可能同时观察多份 sidecar（作品/季/集）。
    pub nfo_observation: Option<&'a [crate::metadata::NfoObservation]>,
    pub probe_required: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PublishReport {
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
}
