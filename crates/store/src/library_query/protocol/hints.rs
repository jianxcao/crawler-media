//! Title-only hint search selects references before page hydration.
use crate::{Store, StoreError};
use domain::{ProtocolItemPage, UserId};
use rusqlite::params;
impl Store {
    pub fn protocol_search_hints(
        &self,
        user: UserId,
        term: &str,
        offset: usize,
        limit: usize,
    ) -> Result<ProtocolItemPage, StoreError> {
        let allowed = super::entities::authorized(self, Some(user))?;
        self.with_read(crate::Db::Library,|tx|{
            let ids=serde_json::to_string(&super::entities::visible(tx,&allowed)?)?;
            let search=term.to_lowercase();
            let predicate="i.library_id IN (SELECT value FROM json_each(?1)) AND i.available_file_count>0 AND (instr(i.protocol_title_key,?2)>0 OR instr(i.protocol_original_title_key,?2)>0)";
            let cte=format!("WITH candidates AS (SELECT i.library_id,i.media_id,i.kind,i.preferred_file_id,i.protocol_title_key,ROW_NUMBER() OVER(PARTITION BY i.media_id ORDER BY i.library_id) AS position FROM library_items i WHERE {predicate})");
            let total=tx.query_row(&format!("{cte} SELECT COUNT(*) FROM candidates WHERE position=1"),params![ids,search],|r|r.get(0))?;
            let mut stmt=tx.prepare_cached(&format!("{cte} SELECT library_id,media_id,kind,preferred_file_id FROM candidates WHERE position=1 ORDER BY protocol_title_key,media_id LIMIT ?3 OFFSET ?4"))?;
            let rows=stmt.query_map(params![ids,search,limit as i64,offset as i64],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?)))?.collect::<Result<Vec<_>,_>>()?;
            let items=rows.into_iter().map(|(lib,media,kind,file)|super::entities::reference(tx,&lib,&media,&kind,file)).collect::<Result<_,_>>()?;
            Ok(ProtocolItemPage{items,total})
        })
    }
}
