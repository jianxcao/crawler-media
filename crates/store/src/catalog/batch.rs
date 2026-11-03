use crate::{Store, StoreError};
use std::collections::HashMap;
impl Store {
    /// Exact requested source/key bodies only, on the already bound Cache lease.
    pub fn catalog_cache_for_keys(
        &self,
        source: &str,
        keys: &[String],
    ) -> Result<HashMap<String, String>, StoreError> {
        let table = super::cache_table(source)?;
        self.with_read(crate::Db::Cache, |conn| {
            let mut out = HashMap::new();
            for chunk in keys.chunks(400) {
                let placeholders = vec!["?"; chunk.len()].join(",");
                let mut stmt = conn.prepare(&format!(
                    "SELECT cache_key,body FROM {table} WHERE cache_key IN ({placeholders})"
                ))?;
                for row in stmt.query_map(rusqlite::params_from_iter(chunk), |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })? {
                    let (key, body) = row?;
                    out.insert(key, body);
                }
            }
            Ok(out)
        })
    }
}
