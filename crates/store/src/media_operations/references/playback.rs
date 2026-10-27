//! Playback half of a Media merge: per-user/per-unit favourites, play counts, played
//! assertion and the resume package.

use domain::MediaId;
use rusqlite::{Connection, OptionalExtension, params};

use crate::StoreError;

/// One `playback_units` row. The resume columns are moved as a package.
struct PlaybackRow {
    user_id: String,
    season: i32,
    episode: i32,
    played: i64,
    has_played_assertion: i64,
    played_changed_at_ms: Option<i64>,
    favorite: i64,
    favorite_at: i64,
    play_count: i64,
    watch_activity_at: i64,
    position_ms: i64,
    duration_ms: Option<i64>,
    resume_file_id: Option<String>,
    resume_file_revision: Option<i64>,
    resume_source_version: Option<String>,
    timeline_mode: Option<String>,
    tracks_version: Option<i64>,
    audio_track: Option<String>,
    subtitle_track: Option<String>,
}

const PLAYBACK_COLUMNS: &str = "user_id, season, episode, played, has_played_assertion, \
     played_changed_at_ms, favorite, favorite_at, play_count, watch_activity_at, position_ms, \
     duration_ms, resume_file_id, resume_file_revision, resume_source_version, timeline_mode, \
     tracks_version, audio_track, subtitle_track";

fn load_playback(
    tx: &Connection,
    media: MediaId,
    user_id: &str,
    season: i32,
    episode: i32,
) -> Result<Option<PlaybackRow>, StoreError> {
    let sql = format!(
        "SELECT {PLAYBACK_COLUMNS} FROM playback_units
         WHERE user_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4"
    );
    tx.query_row(
        &sql,
        params![user_id, media.to_string(), season, episode],
        |r| {
            Ok(PlaybackRow {
                user_id: r.get(0)?,
                season: r.get(1)?,
                episode: r.get(2)?,
                played: r.get(3)?,
                has_played_assertion: r.get(4)?,
                played_changed_at_ms: r.get(5)?,
                favorite: r.get(6)?,
                favorite_at: r.get(7)?,
                play_count: r.get(8)?,
                watch_activity_at: r.get(9)?,
                position_ms: r.get(10)?,
                duration_ms: r.get(11)?,
                resume_file_id: r.get(12)?,
                resume_file_revision: r.get(13)?,
                resume_source_version: r.get(14)?,
                timeline_mode: r.get(15)?,
                tracks_version: r.get(16)?,
                audio_track: r.get(17)?,
                subtitle_track: r.get(18)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

/// Merge one user/unit pair. Favourite is OR, play count is max (never summed),
/// the played assertion is a tri-state over rows that actually asserted it, and
/// the resume package comes whole from the later `watch_activity_at` row.
fn merge_playback_rows(survivor: PlaybackRow, loser: PlaybackRow) -> PlaybackRow {
    let has_assertion = survivor.has_played_assertion == 1 || loser.has_played_assertion == 1;
    let (played, played_changed_at_ms) = if has_assertion {
        (
            i64::from(survivor.played == 1 || loser.played == 1),
            survivor
                .played_changed_at_ms
                .into_iter()
                .chain(loser.played_changed_at_ms)
                .max(),
        )
    } else {
        (0, None)
    };
    let favorite = i64::from(survivor.favorite == 1 || loser.favorite == 1);
    let favorite_at = survivor.favorite_at.max(loser.favorite_at);
    let play_count = survivor.play_count.max(loser.play_count);
    let watch_activity_at = survivor.watch_activity_at.max(loser.watch_activity_at);
    let resume = if loser.watch_activity_at > survivor.watch_activity_at {
        loser
    } else {
        survivor
    };
    PlaybackRow {
        user_id: resume.user_id.clone(),
        season: resume.season,
        episode: resume.episode,
        played,
        has_played_assertion: i64::from(has_assertion),
        played_changed_at_ms,
        favorite,
        favorite_at,
        play_count,
        watch_activity_at,
        position_ms: resume.position_ms,
        duration_ms: resume.duration_ms,
        resume_file_id: resume.resume_file_id,
        resume_file_revision: resume.resume_file_revision,
        resume_source_version: resume.resume_source_version,
        timeline_mode: resume.timeline_mode,
        tracks_version: resume.tracks_version,
        audio_track: resume.audio_track,
        subtitle_track: resume.subtitle_track,
    }
}

fn store_playback(tx: &Connection, media: MediaId, row: &PlaybackRow) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE playback_units SET played = ?5, has_played_assertion = ?6,
             played_changed_at_ms = ?7, favorite = ?8, favorite_at = ?9, play_count = ?10,
             watch_activity_at = ?11, position_ms = ?12, duration_ms = ?13,
             resume_file_id = ?14, resume_file_revision = ?15, resume_source_version = ?16,
             timeline_mode = ?17, tracks_version = ?18, audio_track = ?19, subtitle_track = ?20
         WHERE user_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4",
        params![
            row.user_id,
            media.to_string(),
            row.season,
            row.episode,
            row.played,
            row.has_played_assertion,
            row.played_changed_at_ms,
            row.favorite,
            row.favorite_at,
            row.play_count,
            row.watch_activity_at,
            row.position_ms,
            row.duration_ms,
            row.resume_file_id,
            row.resume_file_revision,
            row.resume_source_version,
            row.timeline_mode,
            row.tracks_version,
            row.audio_track,
            row.subtitle_track,
        ],
    )?;
    Ok(())
}

pub(in crate::media_operations) fn merge_playback_units(
    tx: &Connection,
    survivor: MediaId,
    loser: MediaId,
) -> Result<(), StoreError> {
    let keys: Vec<(String, i32, i32)> = {
        let mut stmt =
            tx.prepare("SELECT user_id, season, episode FROM playback_units WHERE media_id = ?1")?;
        stmt.query_map(params![loser.to_string()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?
    };
    for (user_id, season, episode) in keys {
        let loser_row = load_playback(tx, loser, &user_id, season, episode)?.ok_or_else(|| {
            StoreError::Invalid(format!(
                "playback row for {user_id} ({season},{episode}) vanished during merge"
            ))
        })?;
        match load_playback(tx, survivor, &user_id, season, episode)? {
            None => {
                tx.execute(
                    "UPDATE playback_units SET media_id = ?1
                     WHERE user_id = ?2 AND media_id = ?3 AND season = ?4 AND episode = ?5",
                    params![
                        survivor.to_string(),
                        user_id,
                        loser.to_string(),
                        season,
                        episode
                    ],
                )?;
            }
            Some(survivor_row) => {
                let merged = merge_playback_rows(survivor_row, loser_row);
                store_playback(tx, survivor, &merged)?;
                tx.execute(
                    "DELETE FROM playback_units
                     WHERE user_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4",
                    params![user_id, loser.to_string(), season, episode],
                )?;
            }
        }
    }
    Ok(())
}

