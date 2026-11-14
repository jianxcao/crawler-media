use crate::{DownloaderId, FilterId, LibraryId, MediaId, SubscribeId, UserId};
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchMode {
    Search,
    Rss,
    Both,
}

impl FetchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Rss => "rss",
            Self::Both => "both",
        }
    }
}

impl FromStr for FetchMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "search" => Ok(Self::Search),
            "rss" => Ok(Self::Rss),
            "both" => Ok(Self::Both),
            other => Err(other.to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Coverage {
    Movie,
    Tv {
        season: u32,
        episode_from: u32,
        episode_to: Option<u32>,
    },
}

impl Coverage {
    /// Maximum bounded TV window accepted at the API boundary.
    pub const MAX_EPISODES: u32 = 1000;

    /// Validate a persisted Subscribe window before it can drive allocation or selection.
    pub fn validate(&self, full_season_pack: bool) -> Result<(), &'static str> {
        match self {
            Coverage::Movie if full_season_pack => Err("full_season_pack requires TV coverage"),
            Coverage::Movie => Ok(()),
            Coverage::Tv {
                season,
                episode_from,
                episode_to,
            } => {
                if *season == 0 || *episode_from == 0 {
                    return Err("TV season and first episode must be greater than zero");
                }
                if let Some(to) = episode_to {
                    if *to < *episode_from {
                        return Err("TV episode_to must be greater than or equal to episode_from");
                    }
                    if *to - *episode_from >= Self::MAX_EPISODES {
                        return Err("TV coverage cannot exceed 1000 episodes");
                    }
                } else if full_season_pack {
                    return Err("full_season_pack requires bounded TV coverage");
                }
                Ok(())
            }
        }
    }

    /// 覆盖范围内的全部单元 `(season, episode)`；movie = 单个 `(None, None)`。
    /// 与 subscribe_facts / subscribe_wanted 的键同构（-1 表示 movie）。
    pub fn units(&self) -> Vec<(Option<u32>, Option<u32>)> {
        match self {
            Coverage::Movie => vec![(None, None)],
            Coverage::Tv {
                season,
                episode_from,
                episode_to,
            } => {
                let to = episode_to.unwrap_or(*episode_from).max(*episode_from);
                let capped = episode_from.saturating_add(Self::MAX_EPISODES.saturating_sub(1));
                (*episode_from..=to.min(capped))
                    .map(|episode| (Some(*season), Some(episode)))
                    .collect()
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subscribe {
    pub id: SubscribeId,
    pub user_id: UserId,
    pub media_id: MediaId,
    pub coverage: Coverage,
    pub fetch_mode: FetchMode,
    pub filter_id: FilterId,
    pub wash_cut: bool,
    pub wash_cut_filter_id: Option<FilterId>,
    /// 洗版时保留被替换的旧版本（同时留 1080p 与 4K）；默认替换即删。
    pub keep_old_versions: bool,
    pub full_season_pack: bool,
    pub downloader_id: Option<DownloaderId>,
    /// Target library id to transfer/organize media into. If None, falls back to default library or match rules.
    pub library_id: Option<LibraryId>,
    /// "active" | "paused" — persisted tracking state.
    pub tracking_state: String,
    /// Whether the coverage window extends as future episodes air.
    pub follow_future: bool,
    /// Seconds between SubscribeSearch runs. Default 1800 (30 minutes).
    pub search_interval_secs: u32,
}
