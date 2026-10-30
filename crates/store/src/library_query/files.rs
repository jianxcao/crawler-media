use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, LibraryId,
    MediaId, MediaKind, QualitySource,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
struct FileCursor {
    library_id: LibraryId,
    media_id: MediaId,
    order: String,
    after_file_id: LedgerId,
}

const FILE_CURSOR_ORDER: &str = "file_id_asc";

use super::super::{Store, StoreError};
use super::types::FilePage;

impl Store {
    pub fn library_file_page(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        after: Option<&str>,
        limit: usize,
    ) -> Result<FilePage, StoreError> {
        self.with_library_read(|db| db.library_file_page(library_id, media_id, after, limit))
    }
}

#[path = "files/connection.rs"]
mod connection;
