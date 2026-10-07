use domain::LedgerRow;

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

pub fn media_source(row: &LedgerRow) -> MediaSource {
    MediaSource {
        id: compact_id(&row.id.to_string()),
        path: row.path.clone(),
        supports_direct_play: true,
        supports_transcoding: false,
    }
}

impl Item {
    pub fn from_ledger(name: &str, row: &LedgerRow) -> Self {
        Self {
            id: compact_id(&row.id.to_string()),
            name: name.to_string(),
            path: row.path.clone(),
        }
    }
}

fn compact_id(id: &str) -> String {
    id.chars().filter(|c| *c != '-').collect()
}
