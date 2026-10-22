use std::path::{Path, PathBuf};
use std::str::FromStr;

use domain::MediaKind;
use rusqlite::params;

use super::{Store, StoreError};
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryRoot {
    pub id: String,
    pub kind: MediaKind,
    pub path: PathBuf,
    /// Primary root of the kind's default Library (library `is_default` and
    /// `sort_order = 0`). Additional roots of that Library, and every root of a
    /// non-default Library, are reported as `false` (the directory surface's
    /// "extra roots").
    pub is_default: bool,
    /// Owning library id (null until backfilled / assigned).
    pub library_id: Option<String>,
    /// Position within the owning library; 0 = primary root.
    pub sort_order: i32,
    /// Only enabled roots participate in path ownership.
    pub enabled: bool,
}

impl Store {
    pub fn list_library_roots(&self) -> Result<Vec<LibraryRoot>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT r.root_id, l.kind, r.display_path,
                    (l.is_default = 1 AND r.sort_order = 0) AS primary_root,
                    r.library_id, r.sort_order, r.enabled
             FROM library_roots r
             JOIN libraries l ON r.library_id = l.library_id
             ORDER BY l.is_default DESC, r.sort_order, r.display_path",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)? != 0,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i32>(5)?,
                row.get::<_, i64>(6)? != 0,
            ))
        })?;
        let mut roots = Vec::new();
        for row in rows {
            let (id, kind, path, is_default, library_id, sort_order, enabled) = row?;
            roots.push(LibraryRoot {
                id,
                kind: MediaKind::from_str(&kind).map_err(StoreError::MediaKind)?,
                path: PathBuf::from(path),
                is_default,
                library_id,
                sort_order,
                enabled,
            });
        }
        Ok(roots)
    }

    /// Insert a root attached to the kind's default library (extra roots added
    /// through the directory surface land on the default library).
    pub fn insert_library_root(
        &self,
        kind: MediaKind,
        path: &str,
    ) -> Result<LibraryRoot, StoreError> {
        let id = uuid::Uuid::new_v4().to_string();
        let library = self
            .default_library(kind)?
            .ok_or_else(|| StoreError::Missing(format!("default {} library", kind.as_str())))?;
        self.ensure_library_mutable(&library.id)?;
        self.ensure_removal_roots_available(&[path])?;
        let sort_order = self.roots_of(&library.id)?.len() as i32;
        let tx = self.library.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO library_roots (root_id, library_id, path_key, display_path, enabled, sort_order)
             VALUES (?1, ?2, ?5, ?3, 1, ?4)",
            params![
                id,
                library.id,
                path,
                sort_order,
                crate::path_key::canonical_root_key(path)?
            ],
        )?;
        crate::path_key::reload_root_aliases(&tx)?;
        Self::reassign_files_under_root(&tx, &library.id, kind, path)?;
        tx.commit()?;
        Ok(LibraryRoot {
            id,
            kind,
            path: PathBuf::from(path),
            is_default: false,
            library_id: Some(library.id),
            sort_order,
            enabled: true,
        })
    }

    pub fn get_library_root(&self, id: &str) -> Result<Option<LibraryRoot>, StoreError> {
        Ok(self
            .list_library_roots()?
            .into_iter()
            .find(|root| root.id == id))
    }

    pub fn delete_extra_library_root(&self, id: &str) -> Result<bool, StoreError> {
        let root = match self.get_library_root(id)? {
            Some(r) => r,
            None => return Ok(false),
        };
        if root.is_default {
            return Ok(false);
        }
        if let Some(owner) = &root.library_id {
            self.ensure_library_mutable(owner)?;
        }
        let n = self
            .library
            .execute("DELETE FROM library_roots WHERE root_id = ?1", params![id])?;
        Ok(n > 0)
    }
}

impl Store {
    pub(crate) fn reassign_files_under_root(
        tx: &rusqlite::Connection,
        _library_id: &str,
        kind: MediaKind,
        root_path: &str,
    ) -> Result<(), StoreError> {
        let roots = {
            let mut stmt = tx.prepare(
                "SELECT r.root_id, r.library_id, r.display_path
                 FROM library_roots r JOIN libraries l ON l.library_id = r.library_id
                 WHERE l.kind = ?1 AND l.lifecycle_state = 'ready'
                   AND r.enabled = 1
                   AND NOT EXISTS (SELECT 1 FROM library_removals m
                                   WHERE m.library_id = r.library_id AND m.state <> 'succeeded')",
            )?;
            stmt.query_map(params![kind.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
        };
        let root = Path::new(root_path);
        let candidates = {
            let mut stmt = tx.prepare(
                "SELECT file_id, library_id, media_id, path_key FROM library_files
                 WHERE kind = ?1",
            )?;
            stmt.query_map(params![kind.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
        };
        let mut moves = Vec::new();
        for (file_id, old_library, media_id, path) in candidates {
            let file_path = Path::new(&path);
            if !file_path.starts_with(root) {
                continue;
            }
            let best = roots
                .iter()
                .filter(|(_, _, candidate)| file_path.starts_with(Path::new(candidate)))
                .max_by_key(|(_, _, candidate)| Path::new(candidate).components().count());
            let Some((root_id, new_library, _)) = best else {
                continue;
            };
            if old_library != *new_library || !file_root_id_is(tx, &file_id, root_id)? {
                moves.push((
                    file_id,
                    old_library,
                    media_id,
                    root_id.clone(),
                    new_library.clone(),
                ));
            }
        }
        let mut affected = HashSet::new();
        for (file_id, old_library, media_id, root_id, new_library) in moves {
            tx.execute(
                "UPDATE library_files SET library_id = ?2, root_id = ?3 WHERE file_id = ?1",
                params![file_id, new_library, root_id],
            )?;
            tx.execute(
                "UPDATE library_file_units SET library_id = ?2 WHERE file_id = ?1",
                params![file_id, new_library],
            )?;
            affected.insert((old_library, media_id.clone()));
            affected.insert((new_library, media_id));
        }
        for (library, media) in affected {
            super::library_write::reduce::reduce_library_media(
                tx,
                library.parse()?,
                media.parse()?,
            )?;
        }
        Ok(())
    }
}

fn file_root_id_is(
    tx: &rusqlite::Connection,
    file_id: &str,
    root_id: &str,
) -> Result<bool, StoreError> {
    let current: String = tx.query_row(
        "SELECT root_id FROM library_files WHERE file_id = ?1",
        params![file_id],
        |row| row.get(0),
    )?;
    Ok(current == root_id)
}
