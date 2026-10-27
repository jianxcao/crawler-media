use domain::MediaId;

use crate::{Store, StoreError};

impl Store {
    pub fn retired_media_survivors(&self) -> Result<Vec<(MediaId, MediaId)>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT media_id, survivor_id FROM media WHERE identity_state = 'retired' AND survivor_id IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            let loser_str: String = r.get(0)?;
            let survivor_str: String = r.get(1)?;
            Ok((
                loser_str.parse().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                survivor_str.parse().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}
