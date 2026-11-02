use crate::MediaId;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum MediaKind {
    Movie,
    Tv,
    /// 「其他」库：不识别、不刮削的单本视频（文件名即标题）。
    Video,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Movie => "movie",
            Self::Tv => "tv",
            Self::Video => "video",
        }
    }
}

impl FromStr for MediaKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "movie" => Ok(Self::Movie),
            "tv" => Ok(Self::Tv),
            "video" => Ok(Self::Video),
            other => Err(other.to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Media {
    pub id: MediaId,
    pub kind: MediaKind,
    pub title: String,
    pub year: Option<u16>,
    pub original_title: Option<String>,
    /// 来自目录的其他译名。搜索前还会再过门槛，这里按详情顺序原样保存。
    pub aliases: Vec<String>,
    pub tmdb_id: Option<String>,
    pub douban_id: Option<String>,
    pub tvdb_id: Option<String>,
    pub bangumi_id: Option<String>,
    pub anilist_id: Option<String>,
}
