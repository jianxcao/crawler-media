//! Fixed-width UTC timestamps at the HTTP boundary.
use crate::store::StoreError;

pub(crate) fn format(milliseconds: i64) -> Result<String, StoreError> {
    let date =
        time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(milliseconds) * 1_000_000)
            .map_err(|error| {
                StoreError::Invalid(format!("Subscribe timestamp {milliseconds}: {error}"))
            })?;
    if !(0..=9999).contains(&date.year()) {
        return Err(StoreError::Invalid(format!(
            "Subscribe timestamp {milliseconds}: year outside 0000..9999"
        )));
    }
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        date.year(),
        u8::from(date.month()),
        date.day(),
        date.hour(),
        date.minute(),
        date.second(),
        date.millisecond()
    ))
}

pub(super) fn load(
    store: &crate::Store,
    id: domain::SubscribeId,
) -> Result<(String, String), StoreError> {
    let (created, updated) = store.subscribe_times(id)?;
    Ok((format(created)?, format(updated)?))
}
