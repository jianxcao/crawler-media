//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::SubscribeConnection<'_> {
    pub(crate) fn subscribed_media(
        &self,
        candidates: &HashSet<MediaId>,
    ) -> Result<HashSet<MediaId>, StoreError> {
        let mut kept = HashSet::new();
        let mut stmt = self
            .subscribe
            .prepare_cached("SELECT 1 FROM subscribes WHERE media_id = ?1 LIMIT 1")?;
        for id in candidates {
            if stmt.exists([id.to_string()])? {
                kept.insert(*id);
            }
        }
        Ok(kept)
    }
}
