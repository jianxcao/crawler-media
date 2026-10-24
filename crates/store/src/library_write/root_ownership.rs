use std::path::Path;

use domain::LibraryFile;
use rusqlite::{Connection, OptionalExtension, params};

use crate::StoreError;

/// The declared root must exist for this Library and own the path. A root-relative
/// path is joined onto the root (returned as an owned copy); `..` is never accepted.
pub(crate) fn ensure_root_owns_path(
    tx: &Connection,
    file: &LibraryFile,
) -> Result<Option<LibraryFile>, StoreError> {
    let root: Option<String> = tx
        .query_row(
            "SELECT path_key FROM library_roots WHERE root_id = ?1 AND library_id = ?2 AND enabled=1",
            params![file.root_id, file.library_id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    let Some(root) = root else {
        return Err(StoreError::Invalid(format!(
            "root {} does not belong to library {}",
            file.root_id, file.library_id
        )));
    };
    if file
        .path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(StoreError::Invalid(format!(
            "path {} contains `..`",
            file.path.display()
        )));
    }
    let absolutized = if !file.path.is_absolute() {
        let mut owned = file.clone();
        owned.path = Path::new(&root).join(&file.path);
        Some(owned)
    } else {
        None
    };
    let file = absolutized.as_ref().unwrap_or(file);
    let file_key = crate::path_key::canonical_recorded_key(tx, &file.path.to_string_lossy());
    let root_key = crate::path_key::canonical_key(&root, &crate::path_key::table_for(tx));
    let under_root = Path::new(&file_key).starts_with(Path::new(&root_key));
    if !under_root {
        return Err(StoreError::Invalid(format!(
            "path {} is outside root {root}",
            file.path.display()
        )));
    }
    // A nested root created after this scope was prepared owns the path now.
    // The declared root still contains it, but publication must follow the longest one.
    let mut statement = tx.prepare(
        "SELECT r.path_key FROM library_roots r JOIN libraries l ON l.library_id=r.library_id
         WHERE r.enabled=1 AND l.kind=?2 AND length(r.path_key)>length(?1)",
    )?;
    let longer: Vec<String> = statement
        .query_map(params![root_key, file.kind.as_str()], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    if longer
        .iter()
        .any(|candidate| file_key == *candidate || file_key.starts_with(&(candidate.clone() + "/")))
    {
        tracing::error!(file_id = %file.file_id, path = %file.path.display(),
            root_id = %file.root_id, "file publication rejected: a nested root owns the path");
        return Err(StoreError::Protected(format!(
            "path {} belongs to a nested root",
            file.path.display()
        )));
    }
    Ok(absolutized)
}
