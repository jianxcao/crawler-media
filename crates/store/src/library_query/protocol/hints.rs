//! Title-only hint search selects references before page hydration.
use crate::{Store, StoreError};
use domain::{ProtocolItemPage, UserId};
use rusqlite::{params_from_iter, types::Value};

impl Store {
    pub fn protocol_search_hints(
        &self,
        user: UserId,
        term: &str,
        offset: usize,
        limit: usize,
    ) -> Result<ProtocolItemPage, StoreError> {
        let allowed = super::entities::authorized(self, Some(user))?;
        self.with_read(crate::Db::Library, |tx| {
            let ids = serde_json::to_string(&super::entities::visible(tx, &allowed)?)?;
            let mut values = vec![Value::Text(ids), Value::Text(term.to_lowercase())];
            // The index includes people and overview, so the title-only residual
            // remains authoritative. Short terms retain substring semantics.
            let candidate = if let Some(phrase) = crate::library_search::fts_phrase(term) {
                values.push(Value::Text(phrase));
                format!(
                    "i.search_rowid IN ({}) AND ",
                    crate::library_search::fts_rowids("?3")
                )
            } else {
                String::new()
            };
            let cte = format!(
                "WITH candidates AS (SELECT i.library_id,i.media_id,i.kind,i.preferred_file_id,\
                 i.protocol_title_key,ROW_NUMBER() OVER(PARTITION BY i.media_id ORDER BY i.library_id) AS position \
                 FROM library_items i WHERE {candidate}\
                 i.library_id IN (SELECT value FROM json_each(?1)) AND i.available_file_count>0 \
                 AND (instr(i.protocol_title_key,?2)>0 OR instr(i.protocol_original_title_key,?2)>0))"
            );
            let total = tx.query_row(
                &format!("{cte} SELECT COUNT(*) FROM candidates WHERE position=1"),
                params_from_iter(&values),
                |r| r.get(0),
            )?;
            let limit_parameter = values.len() + 1;
            let offset_parameter = limit_parameter + 1;
            values.extend([Value::Integer(limit as i64), Value::Integer(offset as i64)]);
            let mut stmt = tx.prepare_cached(&format!(
                "{cte} SELECT library_id,media_id,kind,preferred_file_id FROM candidates \
                 WHERE position=1 ORDER BY protocol_title_key,media_id \
                 LIMIT ?{limit_parameter} OFFSET ?{offset_parameter}"
            ))?;
            let rows = stmt
                .query_map(params_from_iter(&values), |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let items = rows
                .into_iter()
                .map(|(lib, media, kind, file)| {
                    super::entities::reference(tx, &lib, &media, &kind, file)
                })
                .collect::<Result<_, _>>()?;
            Ok(ProtocolItemPage { items, total })
        })
    }
}
