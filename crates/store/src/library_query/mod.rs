pub mod details;
pub mod facets;
pub mod files;
pub mod items;
pub(crate) mod members;
pub mod page_inputs;
pub mod playback_views;
pub mod protocol;
pub mod series_scalar;
pub mod types;

pub use details::{AdoptedFilterFacts, library_display_locale};
pub use playback_views::{FavoriteOrder, PlaybackViewPage, PlaybackViewQuery, PlaybackViewRow};
pub use items::page::IndexBucket;
pub use types::*;

impl crate::Store {
    pub fn ensure_library_query_ready(
        &self,
        library_id: domain::LibraryId,
    ) -> Result<(), crate::StoreError> {
        items::page::ensure_library_query_ready_on(&self.library, library_id)
    }
}
