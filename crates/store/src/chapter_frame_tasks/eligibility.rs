//! Expected lifecycle barriers affect one obligation, never the whole dispatcher.
use rusqlite::{Connection, OptionalExtension};

use super::connection::{next_attempt, save};
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Eligibility {
    Ready,
    Deferred,
    Fenced(ChapterFrameState),
}

pub(super) fn inspect(db: &Connection, task: &ChapterFrameTask) -> Result<Eligibility, StoreError> {
    let row = db.query_row(
        "SELECT library_id,root_id,media_id,kind,display_path,source_version,presence_state,publication_state
         FROM library_files WHERE file_id=?1", [task.file_id.to_string()], |row| {
            Ok((row.get::<_,String>(0)?, row.get::<_,String>(1)?, row.get::<_,String>(2)?,
                row.get::<_,String>(3)?, row.get::<_,String>(4)?, row.get::<_,String>(5)?,
                row.get::<_,String>(6)?, row.get::<_,String>(7)?))
        },
    ).optional()?;
    let Some((library, root, media, kind, path, source, presence, publication)) = row else {
        return Ok(Eligibility::Fenced(ChapterFrameState::Cancelled));
    };
    if source != task.source_version {
        return Ok(Eligibility::Fenced(ChapterFrameState::Superseded));
    }
    if presence != "present" || publication != "published" {
        return Ok(Eligibility::Fenced(ChapterFrameState::Cancelled));
    }
    let media = media.parse()?;
    if let Err(error) = crate::library_write::publish::ensure_media_identity_writable(db, media) {
        return match error {
            StoreError::Protected(_) | StoreError::Missing(_) => {
                Ok(Eligibility::Fenced(ChapterFrameState::Cancelled))
            }
            error => Err(error),
        };
    }
    let enabled: bool = db
        .query_row(
            "SELECT extract_chapter_images FROM libraries WHERE library_id=?1",
            [&library],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false);
    let kind = match kind.as_str() {
        "movie" => domain::MediaKind::Movie,
        "tv" => domain::MediaKind::Tv,
        "video" => domain::MediaKind::Video,
        _ => return Err(StoreError::Invalid("invalid file kind".into())),
    };
    let owner = crate::library_key_owner::owning_root_key(db, std::path::Path::new(&path), kind)?;
    if !enabled || owner.as_ref() != Some(&(library.clone(), root)) {
        return Ok(Eligibility::Fenced(ChapterFrameState::Cancelled));
    }
    if let Err(error) = crate::library_write::ensure_library_writable(db, &library) {
        return match error {
            StoreError::Protected(_) | StoreError::Missing(_) => library_barrier(db, &library),
            error => Err(error),
        };
    }
    match crate::library_operations::integrity_guard::ensure_media_integrity_writable(db, media) {
        Ok(()) => Ok(Eligibility::Ready),
        Err(StoreError::Protected(_)) => Ok(Eligibility::Deferred),
        Err(error) => Err(error),
    }
}

fn library_barrier(db: &Connection, library: &str) -> Result<Eligibility, StoreError> {
    let integrity: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM libraries l JOIN library_operations o
         ON o.library_id=l.library_id WHERE l.library_id=?1 AND l.lifecycle_state='reindexing'
         AND o.kind='integrity' AND o.state<>'succeeded')",
        [library],
        |row| row.get(0),
    )?;
    Ok(if integrity {
        Eligibility::Deferred
    } else {
        Eligibility::Fenced(ChapterFrameState::Cancelled)
    })
}

pub(super) fn assess(
    db: &Connection,
    snapshot: &mut ChapterFrameTaskSnapshot,
) -> Result<Eligibility, StoreError> {
    let outcome = inspect(db, &snapshot.task)?;
    if let Eligibility::Fenced(state) = outcome {
        snapshot.status.state = state;
        snapshot.status.attempt = next_attempt(snapshot.status.attempt)?;
        save(db, snapshot)?;
    }
    Ok(outcome)
}
