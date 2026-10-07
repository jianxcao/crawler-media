use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    SubscribeSearch,
    SubscribeRss,
    Transfer,
    Scrape,
    CheckIn,
    CatalogRefresh,
    WatchIntake,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SubscribeSearch => "subscribe_search",
            Self::SubscribeRss => "subscribe_rss",
            Self::Transfer => "transfer",
            Self::Scrape => "scrape",
            Self::CheckIn => "check_in",
            Self::CatalogRefresh => "catalog_refresh",
            Self::WatchIntake => "watch_intake",
        }
    }
}

impl FromStr for JobKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "subscribe_search" => Ok(Self::SubscribeSearch),
            "subscribe_rss" => Ok(Self::SubscribeRss),
            "transfer" => Ok(Self::Transfer),
            "scrape" => Ok(Self::Scrape),
            "check_in" => Ok(Self::CheckIn),
            "catalog_refresh" => Ok(Self::CatalogRefresh),
            "watch_intake" => Ok(Self::WatchIntake),
            other => Err(other.to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Schedule {
    Interval { secs: u64 },
}

impl Schedule {
    pub fn as_str(&self) -> String {
        match self {
            Self::Interval { secs } => format!("interval:{secs}"),
        }
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        let Some(secs) = raw.strip_prefix("interval:") else {
            return Err(raw.to_string());
        };
        let secs = secs.parse::<u64>().map_err(|_| raw.to_string())?;
        Ok(Self::Interval { secs })
    }

    pub fn next_after(&self, now: i64) -> i64 {
        match self {
            Self::Interval { secs } => now + i64::try_from(*secs).unwrap_or(i64::MAX),
        }
    }
}
