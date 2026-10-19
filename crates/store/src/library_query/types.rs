use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};

pub use domain::{
    ProtocolItemPage, ProtocolItemQuery, ProtocolItemRef, ProtocolItemType, ProtocolMarkFilter,
    ProtocolParent, ProtocolSort, ProtocolSortKey,
};
use domain::{LibraryFile, LibraryId, MediaId, MediaKind, UnitKey, UserId};

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
    pub preferred_file_id: Option<domain::LedgerId>,
    pub has_still: bool,
    pub rating: Option<f64>,
    pub studio: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ItemPage {
    pub items: Vec<LibraryItem>,
    pub total: Option<u64>,
    pub next_cursor: Option<String>,
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
