use domain::{LibraryFile, PhysicalSourceId, TypedItemId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaSource {
    pub id: String,
    pub path: String,
    pub supports_direct_play: bool,
    pub supports_transcoding: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub path: String,
}

pub fn media_source(file: &LibraryFile) -> MediaSource {
    let source_id = PhysicalSourceId {
        file_id: file.file_id,
        revision: file.version.revision,
    };
    MediaSource {
        id: source_id.to_wire_string(),
        path: file.path.display().to_string(),
        supports_direct_play: true,
        supports_transcoding: false,
    }
}

impl Item {
    pub fn from_target(logical_id: &TypedItemId, name: &str, file: &LibraryFile) -> Self {
        Self {
            id: logical_id.to_wire_string(),
            name: name.to_string(),
            path: file.path.display().to_string(),
        }
    }

    pub fn from_ledger(name: &str, row: &domain::LedgerRow) -> Self {
        Self {
            id: row.id.to_string(),
            name: name.to_string(),
            path: row.path.clone(),
        }
    }
}
