//! One owned value mapping shared by writes and raw-fact Integrity expectations.
use super::*;
use crate::query_projection::expected::OwnedRow;
use rusqlite::{params_from_iter, types::Value};

pub(super) fn owned(summary: &Summary, user: &str, library: &str, media: MediaId) -> OwnedRow {
    [
        ("user_id", Value::from(user.to_string())),
        ("library_id", library.to_string().into()),
        ("media_id", media.to_string().into()),
        ("seen", i64::from(summary.seen).into()),
        ("played", i64::from(summary.played).into()),
        ("favorite", i64::from(summary.favorite).into()),
        ("last_played_at", summary.last_played_at.into()),
        ("favorite_at", summary.favorite_at.into()),
        ("owned_episode_count", summary.owned_episode_count.into()),
        (
            "completed_episode_count",
            summary.completed_episode_count.into(),
        ),
        ("next_season", summary.next_season.into()),
        ("next_episode", summary.next_episode.into()),
        ("resume_season", summary.resume_season.into()),
        ("resume_episode", summary.resume_episode.into()),
        ("activity_next_season", summary.activity_next_season.into()),
        (
            "activity_next_episode",
            summary.activity_next_episode.into(),
        ),
        ("activity_season", summary.activity_season.into()),
        ("activity_episode", summary.activity_episode.into()),
        ("activity_at", summary.activity_at.into()),
        (
            "activity_present",
            i64::from(summary.activity_present).into(),
        ),
        ("all_watch_at", summary.all_watch_at.into()),
        ("resume_activity_at", summary.resume_activity_at.into()),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value))
    .collect()
}
pub(super) fn write(
    tx: &Connection,
    summary: &Summary,
    user: UserId,
    library: &str,
    media: MediaId,
    revision: i64,
) -> Result<(), StoreError> {
    let mut row = owned(summary, &user.to_string(), library, media);
    row.insert("summary_revision".into(), revision.into());
    let columns = row.keys().cloned().collect::<Vec<_>>().join(",");
    let values = vec!["?"; row.len()].join(",");
    let updates = row
        .keys()
        .map(|key| format!("{key}=excluded.{key}"))
        .collect::<Vec<_>>()
        .join(",");
    tx.execute(
        &format!(
            "INSERT INTO user_library_items({columns}) VALUES({values})
        ON CONFLICT(user_id,library_id,media_id) DO UPDATE SET {updates}"
        ),
        params_from_iter(row.values()),
    )?;
    Ok(())
}
