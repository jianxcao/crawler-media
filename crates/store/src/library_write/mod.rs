use std::path::PathBuf;
use domain::{FileVersion, LedgerId, LibraryFile, LibraryId, UnitKey};

pub mod artwork;
pub mod delete;
pub mod presence;
pub mod probe;
pub mod publish;
pub mod source;
pub mod transfer;
pub mod reduce;
pub mod reduce_incremental;

pub use artwork::*;
pub use publish::*;
pub use reduce::*;
pub use presence::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteScope {
    pub library_id: LibraryId,
    pub config_revision: i64,
}

pub struct FilePublication<'a> {
    pub file: &'a LibraryFile,
    pub units: &'a [UnitKey],
    pub expected_version: Option<&'a FileVersion>,
    pub nfo_observation: Option<&'a crate::metadata::NfoObservation>,
    pub probe_required: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PublishReport {
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
}
