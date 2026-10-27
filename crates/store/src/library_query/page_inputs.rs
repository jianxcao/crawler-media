//! Selected protocol references, captured from one library connection.

use std::collections::{HashMap, HashSet};

use domain::{
    FileCoverage, LedgerId, LibraryFile, Media, MediaId, ProtocolItemRef, TypedItemId, UnitKey,
    UserId,
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::file_reads::PlaybackUnitSnapshot;
use crate::maps::{LIBRARY_FILE_COLUMNS, map_library_file, map_media, parse_id};
use crate::media::MEDIA_SELECT;
use crate::playback::UnitState;
use crate::{Store, StoreError};

#[derive(Default)]
pub struct ProtocolPageInputs {
    pub files: HashMap<LedgerId, LibraryFile>,
    pub media: HashMap<MediaId, Media>,
    pub coverage: Vec<FileCoverage>,
    pub user_items: Vec<PlaybackUnitSnapshot>,
}

impl ProtocolPageInputs {
    pub fn hydrated_rows(&self) -> u64 {
        (self.files.len()
            + self.media.len()
            + self
                .coverage
                .iter()
                .map(|row| row.units.len())
                .sum::<usize>()
            + self.user_items.len()) as u64
    }
}

impl Store {
    /// Validate a complete response against one current Library snapshot. The
    /// contexts preserve typed auth changes before selected-source validation.
    pub fn validate_protocol_references(
        &self,
        contexts: &[crate::LibraryReadContext],
        references: &[ProtocolItemRef],
    ) -> Result<(), crate::LibraryReadError> {
        let context = contexts.first().ok_or_else(|| {
            StoreError::Invalid("protocol response missing authorization context".into())
        })?;
        let tx = self
            .library
            .unchecked_transaction()
            .map_err(StoreError::from)?;
        for context in contexts {
            self.validate_library_read(context)?;
        }
        let authorized = context.authorized();
        let user = context.user();
        let is_admin = context.is_admin();
        if !references.is_empty() {
            let mut checked_libraries = HashSet::new();
            for reference in references {
                let (file, _) = validated_source(
                    &tx,
                    &authorized,
                    user,
                    is_admin,
                    &mut checked_libraries,
                    reference,
                )?;
                let exists: Option<i64> = tx
                    .query_row(
                        "SELECT 1 FROM media WHERE media_id=?1",
                        [file.media_id.to_string()],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(StoreError::from)?;
                if exists.is_none() {
                    return Err(stale(reference, "selected Media missing").into());
                }
            }
        }
        // App credentials/role can change independently of the Library snapshot.
        for context in contexts {
            self.validate_library_read(context)?;
        }
        tx.commit().map_err(StoreError::from)?;
        Ok(())
    }

    pub fn protocol_page_inputs_with_stats(
        &self,
        user: UserId,
        references: &[ProtocolItemRef],
    ) -> Result<(ProtocolPageInputs, super::items::QueryWorkStats), StoreError> {
        let observer = super::items::stats::QueryWorkScope::new(&[&self.app, &self.library]);
        let inputs = self.protocol_page_inputs(user, references)?;
        let mut stats = observer.stats(references.len() as u64);
        stats.hydrated_rows = inputs.hydrated_rows();
        Ok((inputs, stats))
    }

    pub fn protocol_page_inputs(
        &self,
        user: UserId,
        references: &[ProtocolItemRef],
    ) -> Result<ProtocolPageInputs, StoreError> {
        let authorized = self
            .authorized_library_ids(user)?
            .into_iter()
            .map(|id| id.parse())
            .collect::<Result<HashSet<_>, _>>()?;
        let is_admin = self.user_role(user)? == "admin";
        let tx = self.library.unchecked_transaction()?;
        let inputs = page_inputs_on(&tx, &authorized, user, is_admin, references)?;
        tx.commit()?;
        Ok(inputs)
    }
}

/// Borrowed-connection core for the isolated reader. Every selected fact must
/// match its source revision and logical coordinate; no replacement is chosen.
pub(crate) fn page_inputs_on(
    conn: &Connection,
    authorized: &HashSet<domain::LibraryId>,
    user: UserId,
    is_admin: bool,
    references: &[ProtocolItemRef],
) -> Result<ProtocolPageInputs, StoreError> {
    let mut result = ProtocolPageInputs::default();
    if references.is_empty() {
        return Ok(result);
    }
    let mut unit_keys = HashSet::new();
    let mut checked_libraries = HashSet::new();
    for reference in references {
        let (file, unit) = validated_source(
            conn,
            authorized,
            user,
            is_admin,
            &mut checked_libraries,
            reference,
        )?;
        let media_id = file.media_id;
        if let std::collections::hash_map::Entry::Vacant(e) = result.media.entry(media_id) {
            let sql = format!("{MEDIA_SELECT} WHERE m.media_id=?1");
            let media = conn
                .query_row(&sql, [media_id.to_string()], map_media)
                .optional()?
                .ok_or_else(|| stale(reference, "selected Media missing"))?;
            e.insert(media);
        }
        if !result.files.contains_key(&file.file_id) {
            result.coverage.push(FileCoverage {
                path_key: file_path_key(conn, file.file_id)?,
                file: file.clone(),
                units: vec![unit],
            });
            result.files.insert(file.file_id, file);
        } else if let Some(coverage) = result
            .coverage
            .iter_mut()
            .find(|c| c.file.file_id == file.file_id)
            && !coverage.units.contains(&unit) {
                coverage.units.push(unit);
            }
        let (season, episode) = match &reference.item {
            TypedItemId::Unit { key, .. } => (key.season, key.episode),
            _ => (-1, -1),
        };
        for coords in [(season, episode), (season, -1), (-1, -1)] {
            unit_keys.insert((media_id, coords.0, coords.1));
        }
    }
    for (media, season, episode) in unit_keys {
        if let Some(item) = read_user_item(conn, user, media, season, episode)? {
            result.user_items.push(item);
        }
    }
    Ok(result)
}

fn validated_source(
    conn: &Connection,
    authorized: &HashSet<domain::LibraryId>,
    user: UserId,
    is_admin: bool,
    checked_libraries: &mut HashSet<domain::LibraryId>,
    reference: &ProtocolItemRef,
) -> Result<(LibraryFile, UnitKey), StoreError> {
    if !authorized.contains(&reference.library_id) {
        return Err(stale(reference, "Library access changed"));
    }
    if checked_libraries.insert(reference.library_id) {
        validate_library(conn, user, is_admin, reference)?;
    }
    let source = reference
        .preferred_source
        .as_ref()
        .ok_or_else(|| stale(reference, "selected source missing"))?;
    let file = read_file(conn, source.file_id)?
        .ok_or_else(|| stale(reference, "selected file missing"))?;
    if file.library_id != reference.library_id
        || file.media_id != reference.item.media_id()
        || file.version.revision != source.revision
        || file.publication != domain::FilePublication::Published
        || file.presence != domain::FilePresence::Present
    {
        return Err(stale(reference, "selected source changed"));
    }
    let unit = selected_unit(conn, &reference.item, source.file_id)?
        .ok_or_else(|| stale(reference, "selected coverage missing"))?;
    Ok((file, unit))
}

fn validate_library(
    conn: &Connection,
    user: UserId,
    is_admin: bool,
    reference: &ProtocolItemRef,
) -> Result<(), StoreError> {
    let row: Option<(String, String, bool, String, bool)> = conn
        .query_row(
            "SELECT lifecycle_state,access_mode,admin_visible,member_ids_json,
                EXISTS(SELECT 1 FROM library_removals r WHERE r.library_id=l.library_id)
         FROM libraries l WHERE l.library_id=?1",
            [reference.library_id.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get::<_, i64>(2)? != 0,
                    row.get(3)?,
                    row.get::<_, i64>(4)? != 0,
                ))
            },
        )
        .optional()?;
    let Some((state, access, admin_visible, members, removed)) = row else {
        return Err(stale(reference, "selected Library missing"));
    };
    if state == "reindexing" {
        return Err(StoreError::QueryUnavailable {
            library_id: reference.library_id,
        });
    }
    if state != "ready" || removed {
        return Err(stale(reference, "selected Library unavailable"));
    }
    let permitted = if is_admin {
        admin_visible
    } else if access == "everyone" {
        true
    } else {
        serde_json::from_str::<Vec<String>>(&members)
            .unwrap_or_default()
            .contains(&user.to_string())
    };
    if !permitted {
        return Err(stale(reference, "Library access changed"));
    }
    Ok(())
}

fn stale(reference: &ProtocolItemRef, reason: &str) -> StoreError {
    StoreError::Protected(format!(
        "protocol page reference {}: {reason}",
        reference.item.to_wire_string()
    ))
}

fn read_file(conn: &Connection, id: LedgerId) -> Result<Option<LibraryFile>, StoreError> {
    let sql = format!("SELECT {LIBRARY_FILE_COLUMNS} FROM library_files f WHERE f.file_id=?1");
    conn.query_row(&sql, [id.to_string()], map_library_file)
        .optional()
        .map_err(Into::into)
}

fn file_path_key(conn: &Connection, id: LedgerId) -> Result<String, StoreError> {
    conn.query_row(
        "SELECT path_key FROM library_files WHERE file_id=?1",
        [id.to_string()],
        |r| r.get(0),
    )
    .map_err(Into::into)
}

fn selected_unit(
    conn: &Connection,
    item: &TypedItemId,
    id: LedgerId,
) -> Result<Option<UnitKey>, StoreError> {
    let wanted = match item {
        TypedItemId::Unit { key, .. } => Some(*key),
        _ => None,
    };
    let row = match wanted {
        Some(key) => conn.query_row(
            "SELECT season,episode FROM library_file_units WHERE file_id=?1 AND season=?2 AND episode=?3",
            params![id.to_string(), key.season, key.episode],
            |r| Ok(UnitKey::new(r.get(0)?, r.get(1)?)),
        ).optional()?,
        None => conn.query_row(
            "SELECT season,episode FROM library_file_units WHERE file_id=?1 ORDER BY season,episode LIMIT 1",
            [id.to_string()], |r| Ok(UnitKey::new(r.get(0)?, r.get(1)?)),
        ).optional()?,
    };
    Ok(row)
}

fn read_user_item(
    conn: &Connection,
    user: UserId,
    media: MediaId,
    season: i32,
    episode: i32,
) -> Result<Option<PlaybackUnitSnapshot>, StoreError> {
    conn.query_row(
        "SELECT media_id,season,episode,position_ms,played,favorite,duration_ms,audio_track,
                subtitle_track,play_count,watch_activity_at,favorite_at,
                CASE WHEN has_played_assertion=1 THEN COALESCE(played_changed_at_ms,0) END
         FROM playback_units WHERE user_id=?1 AND media_id=?2 AND season=?3 AND episode=?4",
        params![user.to_string(), media.to_string(), season, episode],
        |r| {
            Ok(PlaybackUnitSnapshot {
                media_id: parse_id(r.get(0)?, 0)?,
                season: r.get(1)?,
                episode: r.get(2)?,
                state: UnitState {
                    position_ms: r.get(3)?,
                    played: r.get::<_, i64>(4)? != 0,
                    favorite: r.get::<_, i64>(5)? != 0,
                    duration_ms: r.get(6)?,
                    audio_track: r.get(7)?,
                    subtitle_track: r.get(8)?,
                    play_count: r.get(9)?,
                    updated_at: r.get(10)?,
                    favorite_at: r.get(11)?,
                    played_assertion_at_ms: r.get(12)?,
                },
            })
        },
    )
    .optional()
    .map_err(Into::into)
}
