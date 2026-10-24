use rusqlite::params;

use crate::{Store, StoreError};

/// Root filesystem observations, owned independently of Store. HTTP callers build
/// this before acquiring Store so a slow mount cannot block unrelated DB reads.
pub struct PreparedLibraryRoots {
    pub(crate) entries: Vec<(String, String)>,
}

impl PreparedLibraryRoots {
    pub fn observe(paths: &[&str]) -> Result<Self, StoreError> {
        let mut entries = Vec::with_capacity(paths.len());
        for display in paths.iter().map(|p| p.trim()).filter(|p| !p.is_empty()) {
            let key = crate::path_key::canonical_root_key(display)?;
            if entries.iter().any(|(_, existing)| existing == &key) {
                return Err(StoreError::Invalid(format!("duplicate root {display}")));
            }
            entries.push((display.to_owned(), key));
        }
        Ok(Self { entries })
    }
}

impl Store {
    pub(super) fn replace_roots(
        conn: &rusqlite::Connection,
        library: &str,
        roots: &PreparedLibraryRoots,
    ) -> Result<(), StoreError> {
        let mut statement =
            conn.prepare("SELECT root_id,display_path FROM library_roots WHERE library_id=?1")?;
        let existing = statement
            .query_map([library], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (index, (display, key)) in roots.entries.iter().enumerate() {
            if let Some((id, _)) = existing.iter().find(|(_, path)| path == display) {
                conn.execute(
                    "UPDATE library_roots SET sort_order=?1 WHERE root_id=?2",
                    params![index as i32, id],
                )?;
            } else {
                conn.execute(
                    "INSERT INTO library_roots(root_id,library_id,path_key,display_path,enabled,sort_order)
                     VALUES(?1,?2,?3,?4,1,?5)",
                    params![uuid::Uuid::new_v4().to_string(),library,key,display,index as i32],
                )?;
            }
        }
        for (id, display) in existing {
            if !roots.entries.iter().any(|(path, _)| path == &display) {
                conn.execute("DELETE FROM library_roots WHERE root_id=?1", [id])?;
            }
        }
        Ok(())
    }
}
