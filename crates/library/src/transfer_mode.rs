//! Transfer mode selection from configuration and filesystem placement.
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferMode {
    Hardlink,
    Copy,
    Move,
}

impl TransferMode {
    /// Storage form used by the Library ledger's `transfer_mode` column.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Hardlink => "hardlink",
            Self::Copy => "copy",
            Self::Move => "move",
        }
    }
}

pub fn resolve_mode(src: &Path, dest_dir: &Path, configured: Option<TransferMode>) -> TransferMode {
    if let Some(mode) = configured {
        return mode;
    }
    if same_filesystem(src, dest_dir) {
        TransferMode::Hardlink
    } else {
        TransferMode::Copy
    }
}

fn same_filesystem(src: &Path, dest_dir: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let Ok(src_meta) = src.metadata() else {
            return false;
        };
        let mut ancestor = dest_dir.to_path_buf();
        loop {
            if let Ok(dest_meta) = ancestor.metadata() {
                return src_meta.dev() == dest_meta.dev();
            }
            if !ancestor.pop() {
                return false;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (src, dest_dir);
        false
    }
}
