use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};

pub use domain::{
    ProtocolItemPage, ProtocolItemQuery, ProtocolItemRef, ProtocolItemType, ProtocolMarkFilter,
    ProtocolParent, ProtocolSort, ProtocolSortKey,
};
use domain::{LedgerId, LibraryFile, LibraryId, MediaId, MediaKind, UnitKey, UserId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowseOrder {
    Title,
    AddedAt,
    ReleaseDate,
    Rating,
    Runtime,
    Size,
    Resolution,
    Random,
    LastPlayed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowsePage {
    Offset { start: usize, limit: usize },
    Cursor { after: Option<String>, limit: usize },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BrowseFilter {
    pub genres: Vec<String>,
    pub countries: Vec<String>,
    pub decades: Vec<String>,
    pub runtimes: Vec<String>,
    pub languages: Vec<String>,
    pub resolutions: Vec<String>,
    pub stock: Vec<String>,
    pub hdr: Option<bool>,
    pub min_rating: Option<f64>,
    pub identity: Option<String>,
    pub watch: Option<String>,
    pub unwatched_first: bool,
    pub require_image: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LibraryQuery {
    pub library_id: LibraryId,
    pub user_id: Option<UserId>,
    pub filter: BrowseFilter,
    pub order: BrowseOrder,
    pub descending: bool,
    pub page: BrowsePage,
    pub include_total: bool,
}

/// One card of a library browse page: the `library_items` summary columns a poster wall
/// renders, plus the columns the filter dimensions read. Every value here is a stored
/// fact of the new model — no request-time file observation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LibraryItem {
    pub library_id: LibraryId,
    pub media_id: MediaId,
    pub kind: MediaKind,
    pub title: String,
    pub title_sort: String,
    pub year: Option<u16>,
    pub file_count: usize,
    pub available_file_count: usize,
    pub missing_file_count: usize,
    pub unknown_file_count: usize,
    pub distinct_episode_count: usize,
    pub distinct_season_count: usize,
    pub size_bytes: Option<u64>,
    pub preferred_file_id: Option<LedgerId>,
    pub has_still: bool,
    /// Studio name / logo when the adopted metadata carries them.
    pub studio: Option<String>,
    pub rating: Option<f64>,
    pub studio_logo: Option<String>,
    /// Adopted language of the item, lower-case code when known.
    pub language: Option<String>,
    /// Published file that owns this item's poster (`library_items.poster_ref`); the
    /// artwork route serves `poster_url` from this id, so no card ever stats a file.
    pub poster_ref: Option<LedgerId>,
    pub backdrop_ref: Option<LedgerId>,
    /// `provisional` / `confirmed` / `merging` / `retired` (Media identity state).
    pub identity_state: String,
    /// `library_items.missing`: 1 when any published file is missing.
    pub missing: bool,
    /// Adopted metadata exists for this library+media.
    pub scraped: bool,
    /// Highest published-file resolution rank (`2160p|4k=4, 1080p|1080i=3, 720p=2, else 1`).
    pub resolution_rank: i64,
}

impl LibraryItem {
    pub fn has_poster(&self) -> bool {
        self.poster_ref.is_some()
    }

    pub fn has_backdrop(&self) -> bool {
        self.backdrop_ref.is_some()
    }

    /// `max_resolution` card field, derived from the stored rank so a card never has to
    /// read files (the inverse of the reducer's rank map).
    pub fn max_resolution(&self) -> &'static str {
        match self.resolution_rank {
            4 => "2160p",
            3 => "1080p",
            2 => "720p",
            _ => "480p",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ItemPage {
    pub items: Vec<LibraryItem>,
    pub total: Option<u64>,
    pub next_cursor: Option<String>,
}

/// Everything a poster wall needs per card beyond the `library_items` columns, read for a
/// whole page at once: seasons from `library_units`, studio/logo from `media_metadata`,
/// favourite from `user_library_items`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryItemFacts {
    pub seasons: Vec<u32>,
    pub studio: Option<String>,
    pub studio_logo: Option<String>,
    pub favorite: bool,
}

/// One still image owned by a published library file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryStill {
    pub media_id: MediaId,
    pub file_id: LedgerId,
    /// `None` when the file covers the whole work (movie, or a full-season pack).
    pub season: Option<u32>,
    pub episode: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FacetTier {
    Primary,
    All,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FacetRequest {
    pub library_id: LibraryId,
    pub user_id: Option<UserId>,
    pub filter: BrowseFilter,
    pub tier: FacetTier,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacetCount {
    pub value: String,
    pub label: String,
    pub count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryFacets {
    pub total: u64,
    /// Dimension key (`genres`, `countries`, `decades`, `watch`, `ratings`, `runtimes`,
    /// `languages`, `resolutions`, `hdr`, `stock`) → sorted candidate values.
    pub groups: BTreeMap<String, Vec<FacetCount>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FilePage {
    pub files: Vec<LibraryFile>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LibraryItemDetail {
    pub item: LibraryItem,
    pub metadata: domain::MetadataFields,
    pub files: Vec<LibraryFile>,
    pub next_file_cursor: Option<String>,
    /// All seasons represented by library_units, independent of the first file page.
    pub seasons: Vec<u32>,
    /// Coverage of each listed file (`(-1,-1)` for a whole-work file).
    pub file_units: Vec<(LedgerId, Vec<UnitKey>)>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LibraryUnit {
    pub library_id: LibraryId,
    pub media_id: MediaId,
    pub key: UnitKey,
    pub file_count: usize,
    pub available_file_count: usize,
    pub preferred_file_id: Option<domain::LedgerId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UnitPage {
    pub units: Vec<LibraryUnit>,
    pub total: u64,
    pub next_cursor: Option<String>,
}

/// One episode row of a library item: catalog metadata when adopted, ownership from the
/// file facts. Replaces reading NFO files at request time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LibraryEpisode {
    pub season: u32,
    pub episode: u32,
    pub title: Option<String>,
    pub overview: Option<String>,
    pub air_date: Option<String>,
    pub runtime_minutes: Option<u32>,
    pub rating: Option<f64>,
    pub still_url: Option<String>,
    /// At least one published file covers this unit.
    pub owned: bool,
    pub file_ids: Vec<LedgerId>,
}
