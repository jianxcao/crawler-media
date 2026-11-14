//! Selected Jobs resolve Subscribe targets without per-result reads.
use std::collections::HashMap;

use domain::{MediaId, SubscribeId};

use crate::{Db, Store, StoreError};

impl Store {
    pub fn subscribe_media_batch(
        &self,
        ids: &[SubscribeId],
    ) -> Result<HashMap<SubscribeId, MediaId>, StoreError> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let keys: Vec<_> = ids.iter().map(ToString::to_string).collect();
        let keys = serde_json::to_string(&keys)?;
        self.with_read(Db::Subscribe, |conn| {
            let mut statement = conn.prepare_cached(
                "SELECT id,media_id FROM subscribes WHERE id IN (SELECT value FROM json_each(?1))",
            )?;
            let mut rows = statement.query([keys])?;
            let mut result = HashMap::new();
            while let Some(row) = rows.next()? {
                result.insert(
                    row.get::<_, String>(0)?.parse()?,
                    row.get::<_, String>(1)?.parse()?,
                );
            }
            Ok(result)
        })
    }
}
