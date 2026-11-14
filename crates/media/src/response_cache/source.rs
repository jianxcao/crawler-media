use std::fmt;
use std::str::FromStr;

use super::CacheDbError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CacheSource {
    Tmdb,
    Douban,
    Tvdb,
    Bangumi,
    Anilist,
    Fanart,
}

pub const CACHE_SOURCES: [CacheSource; 6] = [
    CacheSource::Tmdb,
    CacheSource::Douban,
    CacheSource::Tvdb,
    CacheSource::Bangumi,
    CacheSource::Anilist,
    CacheSource::Fanart,
];

impl CacheSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tmdb => "tmdb",
            Self::Douban => "douban",
            Self::Tvdb => "tvdb",
            Self::Bangumi => "bangumi",
            Self::Anilist => "anilist",
            Self::Fanart => "fanart",
        }
    }

    pub const fn table_name(self) -> &'static str {
        match self {
            Self::Tmdb => "tmdb_cache",
            Self::Douban => "douban_cache",
            Self::Tvdb => "tvdb_cache",
            Self::Bangumi => "bangumi_cache",
            Self::Anilist => "anilist_cache",
            Self::Fanart => "fanart_cache",
        }
    }
}

impl fmt::Display for CacheSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for CacheSource {
    type Err = CacheDbError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "tmdb" => Ok(Self::Tmdb),
            "douban" => Ok(Self::Douban),
            "tvdb" => Ok(Self::Tvdb),
            "bangumi" => Ok(Self::Bangumi),
            "anilist" => Ok(Self::Anilist),
            "fanart" => Ok(Self::Fanart),
            other => Err(CacheDbError::UnknownSource(other.to_string())),
        }
    }
}
