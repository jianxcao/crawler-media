pub mod types;
pub use types::*;

mod boost;
mod boost_facts;
mod boost_queries;
mod defers;
mod operations;
mod ownership;
mod retire;

pub use boost_queries::{BoostProtectionSets, OwnedDownloadTaskSummary};

use crate::StoreError;
use crate::plugins::PluginError;

pub(super) fn validate_non_empty(value: &str, msg: &'static str) -> Result<(), StoreError> {
    if value.trim().is_empty() {
        return Err(PluginError::Invalid(msg).into());
    }
    Ok(())
}

pub(super) fn parse_ts_id_cursor(after: Option<&str>) -> Result<Option<(i64, String)>, StoreError> {
    let Some(raw) = after else {
        return Ok(None);
    };
    let Some((ts_raw, id)) = raw.split_once(':') else {
        return Err(PluginError::Invalid("invalid cursor").into());
    };
    let ts: i64 = ts_raw
        .parse()
        .map_err(|_| PluginError::Invalid("invalid cursor timestamp"))?;
    if id.trim().is_empty() {
        return Err(PluginError::Invalid("invalid cursor id").into());
    }
    Ok(Some((ts, id.to_string())))
}

pub(super) fn parse_opt_id<T: std::str::FromStr>(
    raw: Option<String>,
) -> rusqlite::Result<Option<T>> {
    raw.map(|s| s.parse().map_err(|_| rusqlite::Error::InvalidQuery))
        .transpose()
}
