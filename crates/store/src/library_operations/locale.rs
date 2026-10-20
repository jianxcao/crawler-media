//! Display-locale change for a reconfiguration operation.
//!
//! A locale switch only changes which locale new lookups prefer: already adopted and
//! already written NFO inputs keep their explicit language, so nothing is re-translated
//! behind the user's back and no `library_metadata_inputs.locale` row is rewritten.

use rusqlite::{Connection, params};

use super::roots::BatchOutcome;
use crate::StoreError;

pub(crate) fn apply_locale(
    tx: &Connection,
    library: &str,
    locale: Option<&str>,
) -> Result<BatchOutcome, StoreError> {
    let Some(locale) = locale else {
        return Ok(BatchOutcome::Done);
    };
    let trimmed = locale.trim();
    if trimmed.is_empty() {
        return Err(StoreError::Invalid("display locale cannot be empty".into()));
    }
    tx.execute(
        "UPDATE libraries SET display_locale = ?2 WHERE library_id = ?1",
        params![library, trimmed],
    )?;
    // Existing NFO inputs keep their explicit locale: switching the display language
    // must not retroactively re-label sidecar observations.
    let inputs: i64 = tx.query_row(
        "SELECT COUNT(*) FROM library_metadata_inputs WHERE library_id = ?1",
        params![library],
        |r| r.get(0),
    )?;
    tracing::info!(
        library_id = library,
        locale = trimmed,
        preserved_inputs = inputs,
        "library display locale changed; existing NFO inputs keep their language"
    );
    Ok(BatchOutcome::Done)
}
