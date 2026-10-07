use super::ApiError;
use crate::settings_keys;
use domain::FilterId;
use std::str::FromStr;

pub(crate) fn default_filter_id(store: &crate::Store) -> Result<Option<FilterId>, ApiError> {
    let Some(raw) = store.get_setting(settings_keys::DEFAULT_FILTER_ID)? else {
        return Ok(None);
    };
    FilterId::from_str(&raw)
        .map(Some)
        .map_err(|_| ApiError::invalid("filter.invalid", "invalid default Filter id".into()))
}
