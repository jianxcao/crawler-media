pub mod details;
pub mod facets;
pub mod files;
pub mod items;
pub(crate) mod members;
mod page_hydration;
pub mod page_inputs;
mod page_validate;
pub use page_hydration::HydratedFile;
pub use page_validate::{PageSnapshot, PageValidation};
pub mod playback_views;
pub mod protocol;
pub mod series_scalar;
pub mod types;

pub use details::{AdoptedFilterFacts, library_display_locale};
pub use items::page::IndexBucket;
pub use playback_views::{FavoriteOrder, PlaybackViewPage, PlaybackViewQuery, PlaybackViewRow};
pub use types::*;

impl crate::Store {
    pub fn ensure_library_query_ready(
        &self,
        library_id: domain::LibraryId,
    ) -> Result<(), crate::StoreError> {
        self.with_library_read(|db| db.ensure_library_query_ready(library_id))
    }
}

#[path = "connection.rs"]
mod connection;

pub(crate) use details::metadata::{instance_default_locale_on, library_display_locale_on};
