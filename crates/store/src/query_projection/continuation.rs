//! Default continuation coordinate shares every projection settlement path.
use crate::StoreError;
use rusqlite::{Connection, OptionalExtension, params};

pub(super) fn refresh_default_unit(
    tx: &Connection,
    library: &str,
    media: &str,
) -> Result<(), StoreError> {
    let first: Option<(i32,i32)> = tx.query_row(
        "SELECT u.season,u.episode FROM library_units u JOIN library_items i USING(library_id,media_id)
         WHERE u.library_id=?1 AND u.media_id=?2 AND i.kind='tv' AND u.available_file_count>0
           AND u.season>=0 AND u.episode>=0 ORDER BY u.season,u.episode LIMIT 1",
        params![library,media], |row| Ok((row.get(0)?,row.get(1)?))).optional()?;
    tx.execute(
        "UPDATE library_items SET first_owned_season=?3,first_owned_episode=?4
        WHERE library_id=?1 AND media_id=?2",
        params![
            library,
            media,
            first.map(|unit| unit.0),
            first.map(|unit| unit.1)
        ],
    )?;
    Ok(())
}
