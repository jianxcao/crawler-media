use crate::{Store, StoreError};
use domain::{Media, MediaId};
use std::collections::{HashMap, HashSet};
impl Store {
    /// Exact identities from the request's already bound Library reader lease.
    pub fn media_for_ids(&self, ids: &[MediaId]) -> Result<HashMap<MediaId, Media>, StoreError> {
        let ids: Vec<_> = ids
            .iter()
            .copied()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        self.with_read(crate::Db::Library, |conn| {
            let mut out = HashMap::new();
            for chunk in ids.chunks(400) {
                let placeholders = vec!["?"; chunk.len()].join(",");
                let mut stmt = conn.prepare(&format!(
                    "{} WHERE m.media_id IN ({placeholders})",
                    super::MEDIA_SELECT
                ))?;
                for row in stmt.query_map(
                    rusqlite::params_from_iter(chunk.iter().map(ToString::to_string)),
                    crate::maps::map_media,
                )? {
                    let media = row?;
                    out.insert(media.id, media);
                }
            }
            Ok(out)
        })
    }
}
