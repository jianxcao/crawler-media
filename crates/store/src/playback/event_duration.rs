//! C06: which duration a playback event is judged against.
//!
//! Clients may omit `duration_ms` (a bare Stop, a Progress from a minimal
//! player). The stored duration is reused only while the event stays on the
//! exact physical source the resume package was bound to: same file, revision,
//! source version and full-file timeline. Another source never inherits it.

use domain::{MediaId, TimelineMode, UnitKey};
use rusqlite::{OptionalExtension, Transaction, params};

use super::types::BoundPlaybackEvent;
use crate::StoreError;

/// The resume binding and duration of one unit, read once inside the event's
/// write transaction before any fact is written.
#[derive(Debug)]
pub(super) struct EventResumeSnapshot {
    file_id: String,
    revision: i64,
    source_version: String,
    timeline: String,
    duration_ms: Option<i64>,
}

pub(super) fn read_event_resume(
    tx: &Transaction<'_>,
    event: &BoundPlaybackEvent,
    media_id: MediaId,
    unit: UnitKey,
) -> Result<Option<EventResumeSnapshot>, StoreError> {
    tx.query_row(
        "SELECT resume_file_id, resume_file_revision, resume_source_version, timeline_mode,
                duration_ms
         FROM playback_units
         WHERE user_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4
           AND resume_file_id IS NOT NULL",
        params![
            event.user_id.to_string(),
            media_id.to_string(),
            unit.season,
            unit.episode
        ],
        |row| {
            Ok(EventResumeSnapshot {
                file_id: row.get(0)?,
                revision: row.get(1)?,
                source_version: row.get(2)?,
                timeline: row.get(3)?,
                duration_ms: row.get(4)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

/// The event's own positive duration, else the stored duration of the same
/// bound source; never a non-positive value (which would auto-complete at 0).
pub(super) fn effective_event_duration(
    event: &BoundPlaybackEvent,
    previous: Option<&EventResumeSnapshot>,
) -> Option<i64> {
    if let Some(duration) = event.duration_ms.filter(|duration| *duration > 0) {
        return Some(duration);
    }
    let previous = previous?;
    let same_source = previous.file_id == event.source_id.file_id.to_string()
        && previous.revision == event.source_id.revision
        && previous.source_version == event.source_version
        && previous.timeline == TimelineMode::FullFile.as_str();
    same_source
        .then_some(previous.duration_ms)
        .flatten()
        .filter(|duration| *duration > 0)
}
