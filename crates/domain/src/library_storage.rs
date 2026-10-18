use std::path::PathBuf;
use serde::{Deserialize, Serialize};

use crate::{Confidence, LedgerId, LibraryId, MediaId, MediaKind, QualitySource, UserId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UnitKey {
    pub season: i32,
    pub episode: i32,
}

impl UnitKey {
    pub const WHOLE: Self = Self {
        season: -1,
        episode: -1,
    };

    pub const fn new(season: i32, episode: i32) -> Self {
        Self { season, episode }
    }

    pub fn is_whole(&self) -> bool {
        self.season == -1 && self.episode == -1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitScope {
    Whole,
    Season(u32),
    Episode { season: u32, episode: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileVersion {
    pub revision: i64,
    pub source_version: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilePresence {
    Present,
    Missing,
    Unknown,
}

impl FilePresence {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Missing => "missing",
            Self::Unknown => "unknown",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "present" => Some(Self::Present),
            "missing" => Some(Self::Missing),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilePublication {
    Pending,
    Published,
}

impl FilePublication {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Published => "published",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "published" => Some(Self::Published),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineMode {
    FullFile,
}

impl TimelineMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FullFile => "full_file",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "full_file" => Some(Self::FullFile),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityScope {
    Work,
    Season(u32),
    Episode { season: u32, episode: u32 },
}

impl EntityScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Work => "work",
            Self::Season(_) => "season",
            Self::Episode { .. } => "episode",
        }
    }

    pub fn season_episode(&self) -> (i32, i32) {
        match self {
            Self::Work => (-1, -1),
            Self::Season(s) => (*s as i32, -1),
            Self::Episode { season, episode } => (*season as i32, *episode as i32),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LibraryFile {
    pub file_id: LedgerId,
    pub library_id: LibraryId,
    pub root_id: String,
    pub media_id: MediaId,
    pub kind: MediaKind,
    pub path: PathBuf,
    pub version: FileVersion,
    pub presence: FilePresence,
    pub publication: FilePublication,
    pub size_bytes: Option<u64>,
    pub stat_known: bool,
    pub created_ns: Option<i64>,
    pub modified_ns: Option<i64>,
    pub source_digest: Option<String>,
    pub source_path: Option<PathBuf>,
    pub transfer_mode: Option<String>,
    pub resolution: Option<String>,
    pub codec: Option<String>,
    pub hdr: Option<String>,
    pub quality_source: QualitySource,
    pub confidence: Confidence,
    pub filter_score: Option<i32>,
    pub release_quality: Option<String>,
    pub duration_ms: Option<i64>,
    pub tracks_version: i64,
    pub poster_path: Option<PathBuf>,
    pub backdrop_path: Option<PathBuf>,
    pub still_path: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileCoverage {
    pub file: LibraryFile,
    pub units: Vec<UnitKey>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSource {
    pub file_id: LedgerId,
    pub destination: PathBuf,
    pub source: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PlayedAssertion {
    pub value: bool,
    pub changed_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeBinding {
    pub file_id: LedgerId,
    pub version: FileVersion,
    pub timeline: TimelineMode,
    pub position_ms: i64,
    pub duration_ms: Option<i64>,
    pub tracks_version: Option<i64>,
    pub audio_track: Option<String>,
    pub subtitle_track: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TypedItemId {
    Work(MediaId),
    Season { media: MediaId, season: u32 },
    Unit { media: MediaId, key: UnitKey },
}

impl TypedItemId {
    pub fn to_wire_string(&self) -> String {
        match self {
            Self::Work(m) => format!("w1_{}", m.as_uuid().simple()),
            Self::Season { media, season } => format!("s1_{}_{season}", media.as_uuid().simple()),
            Self::Unit { media, key } => format!(
                "u1_{}_{}_{}",
                media.as_uuid().simple(),
                key.season,
                key.episode
            ),
        }
    }

    pub fn from_wire_string(s: &str) -> Option<Self> {
        let (prefix, rest) = s.split_once('_')?;
        match prefix {
            "w1" => {
                let uuid = uuid::Uuid::parse_str(rest).ok()?;
                Some(Self::Work(MediaId::from_uuid(uuid)))
            }
            "s1" => {
                let (uuid_str, season_str) = rest.split_once('_')?;
                let uuid = uuid::Uuid::parse_str(uuid_str).ok()?;
                let season = season_str.parse::<u32>().ok()?;
                Some(Self::Season {
                    media: MediaId::from_uuid(uuid),
                    season,
                })
            }
            "u1" => {
                let mut parts = rest.split('_');
                let uuid_str = parts.next()?;
                let season_str = parts.next()?;
                let episode_str = parts.next()?;
                if parts.next().is_some() {
                    return None;
                }
                let uuid = uuid::Uuid::parse_str(uuid_str).ok()?;
                let season = season_str.parse::<i32>().ok()?;
                let episode = episode_str.parse::<i32>().ok()?;
                Some(Self::Unit {
                    media: MediaId::from_uuid(uuid),
                    key: UnitKey { season, episode },
                })
            }
            _ => None,
        }
    }

    pub fn media_id(&self) -> MediaId {
        match self {
            Self::Work(m) => *m,
            Self::Season { media, .. } => *media,
            Self::Unit { media, .. } => *media,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PhysicalSourceId {
    pub file_id: LedgerId,
    pub revision: i64,
}

impl PhysicalSourceId {
    pub fn to_wire_string(&self) -> String {
        format!("f1_{}_{}", self.file_id.as_uuid().simple(), self.revision)
    }

    pub fn from_wire_string(s: &str) -> Option<Self> {
        let (prefix, rest) = s.split_once('_')?;
        if prefix != "f1" {
            return None;
        }
        let (id_str, rev_str) = rest.split_once('_')?;

        let id = LedgerId::from_uuid(uuid::Uuid::parse_str(id_str).ok()?);
        let revision = rev_str.parse::<i64>().ok()?;
        Some(Self {
            file_id: id,
            revision,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolParent {
    Root,
    Library(LibraryId),
    Series(MediaId),
    Season(MediaId, u32),
    Folder { root_id: String },
    Genre(String),
    Studio(String),
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolItemType {
    Movie,
    Series,
    Season,
    Episode,
    Video,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolMarkFilter {
    Favorite,
    Played,
    Unplayed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolSortKey {
    Name,
    DateCreated,
    SeasonNumber,
    EpisodeNumber,
    Noop,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProtocolSort {
    pub key: ProtocolSortKey,
    pub descending: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProtocolItemRef {
    pub library_id: LibraryId,
    pub item: TypedItemId,
    pub preferred_source: Option<PhysicalSourceId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProtocolItemPage {
    pub items: Vec<ProtocolItemRef>,
    pub total: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProtocolItemQuery {
    pub user_id: UserId,
    pub parent: ProtocolParent,
    pub series_id: Option<MediaId>,
    pub season: Option<(MediaId, u32)>,
    pub person_ids: Vec<String>,
    pub studio_ids: Vec<String>,
    pub search_term: Option<String>,
    pub genres: Vec<String>,
    pub genre_ids: Vec<String>,
    pub filters: Vec<ProtocolMarkFilter>,
    pub is_favorite: Option<bool>,
    pub is_played: Option<bool>,
    pub include_types: Vec<ProtocolItemType>,
    pub recursive: bool,
    pub is_folder: Option<bool>,
    pub sort: Vec<ProtocolSort>,
    pub offset: usize,
    pub limit: usize,
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_item_id_wire_roundtrip() {
        let media_id = MediaId::new();
        let work = TypedItemId::Work(media_id);
        let work_wire = work.to_wire_string();
        assert!(work_wire.starts_with("w1_"));
        assert_eq!(TypedItemId::from_wire_string(&work_wire), Some(work));

        let season = TypedItemId::Season { media: media_id, season: 2 };
        let season_wire = season.to_wire_string();
        assert!(season_wire.starts_with("s1_"));
        assert_eq!(TypedItemId::from_wire_string(&season_wire), Some(season));

        let unit = TypedItemId::Unit { media: media_id, key: UnitKey { season: 2, episode: 5 } };
        let unit_wire = unit.to_wire_string();
        assert!(unit_wire.starts_with("u1_"));
        assert_eq!(TypedItemId::from_wire_string(&unit_wire), Some(unit));

        let whole_unit = TypedItemId::Unit { media: media_id, key: UnitKey::WHOLE };
        let whole_wire = whole_unit.to_wire_string();
        assert_eq!(TypedItemId::from_wire_string(&whole_wire), Some(whole_unit));

        assert_eq!(TypedItemId::from_wire_string("invalid"), None);
        assert_eq!(TypedItemId::from_wire_string("w1_not_a_uuid"), None);
    }

    #[test]
    fn physical_source_id_wire_roundtrip() {
        let file_id = LedgerId::new();
        let source_id = PhysicalSourceId { file_id, revision: 42 };
        let wire = source_id.to_wire_string();
        assert!(wire.starts_with("f1_"));
        assert_eq!(PhysicalSourceId::from_wire_string(&wire), Some(source_id));
        assert_eq!(PhysicalSourceId::from_wire_string("f1_invalid"), None);
        assert_eq!(PhysicalSourceId::from_wire_string("x1_uuid_1"), None);
    }

    #[test]
    fn entity_scope_coordinates() {
        assert_eq!(EntityScope::Work.season_episode(), (-1, -1));
        assert_eq!(EntityScope::Season(3).season_episode(), (3, -1));
        assert_eq!(EntityScope::Episode { season: 3, episode: 8 }.season_episode(), (3, 8));
    }
}
