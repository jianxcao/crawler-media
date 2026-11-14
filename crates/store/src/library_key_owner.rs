//! Library ownership from already-registered root keys. No filesystem IO.

use rusqlite::params;

use crate::{Store, StoreError};
use domain::MediaKind;

impl Store {
    /// Library that owns `path` by its registered root keys.
    ///
    /// The caller resolves `path` before taking the Store lock; this only compares keys.
    pub fn library_owning_key(
        &self,
        path: &std::path::Path,
        kind: MediaKind,
    ) -> Result<Option<String>, StoreError> {
        self.with_library_read(|db| db.library_owning_key(path, kind))
    }

    /// Deepest root of the requested kind, including frozen roots so callers cannot
    /// silently publish into a parent when that child refuses work. No filesystem IO.
    pub fn library_root_owning_key(
        &self,
        path: &std::path::Path,
        kind: MediaKind,
    ) -> Result<Option<(String, String)>, StoreError> {
        self.with_library_read(|db| db.library_root_owning_key(path, kind))
    }
}

pub(crate) fn owning_root_key(
    conn: &rusqlite::Connection,
    path: &std::path::Path,
    kind: MediaKind,
) -> Result<Option<(String, String)>, StoreError> {
    let key = crate::path_key::canonical_recorded_key(conn, &path.to_string_lossy());
    let mut stmt = conn.prepare(
        "SELECT r.path_key, l.library_id, r.root_id FROM library_roots r
             JOIN libraries l ON l.library_id = r.library_id
             WHERE l.kind = ?1 AND r.enabled = 1",
    )?;
    let rows = stmt.query_map(params![kind.as_str()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut best: Option<(usize, String, String)> = None;
    let mut ambiguous = false;
    for row in rows {
        let (root, library_id, root_id) = row?;
        if !std::path::Path::new(&key).starts_with(std::path::Path::new(&root)) {
            continue;
        }
        let depth = std::path::Path::new(&root).components().count();
        if best
            .as_ref()
            .is_some_and(|(best_depth, _, _)| depth == *best_depth)
        {
            ambiguous = true;
        }
        if best
            .as_ref()
            .is_none_or(|(best_depth, _, _)| depth > *best_depth)
        {
            best = Some((depth, library_id, root_id));
            ambiguous = false;
        }
    }
    if ambiguous {
        return Err(StoreError::Protected(format!(
            "ambiguous root for {}",
            path.display()
        )));
    }
    Ok(best.map(|(_, library, root)| (library, root)))
}

#[path = "library_key_owner/connection.rs"]
mod connection;
