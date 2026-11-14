//! Pinned season aggregates resolve effective episode assertions without hydration.
use crate::{Store, StoreError};
use domain::{MediaId, UserId};
use rusqlite::params;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolSeason {
    pub season: u32,
    pub total: u64,
    pub played: u64,
}
impl Store {
    pub fn protocol_seasons(
        &self,
        user: UserId,
        media: MediaId,
    ) -> Result<Vec<ProtocolSeason>, StoreError> {
        let allowed = super::entities::authorized(self, Some(user))?;
        self.with_read(crate::Db::Library,|tx|{
            let ids=serde_json::to_string(&super::entities::visible(tx,&allowed)?)?;
            let sql="WITH owned AS (SELECT DISTINCT u.season,u.episode FROM library_units u INDEXED BY units_media_scope
                WHERE u.media_id=?2 AND u.library_id IN (SELECT value FROM json_each(?3)) AND u.available_file_count>0),
                marks AS (SELECT o.season,o.episode,COALESCE((SELECT played FROM (
                    SELECT p.played,p.played_changed_at_ms,p.season,p.episode FROM playback_units p WHERE p.user_id=?1 AND p.media_id=?2 AND p.season=o.season AND p.episode=o.episode AND p.has_played_assertion=1
                    UNION ALL SELECT p.played,p.played_changed_at_ms,p.season,p.episode FROM playback_units p WHERE p.user_id=?1 AND p.media_id=?2 AND p.season=o.season AND p.episode=-1 AND p.has_played_assertion=1
                    UNION ALL SELECT p.played,p.played_changed_at_ms,p.season,p.episode FROM playback_units p WHERE p.user_id=?1 AND p.media_id=?2 AND p.season=-1 AND p.episode=-1 AND p.has_played_assertion=1
                ) ORDER BY played_changed_at_ms DESC,season DESC,episode DESC LIMIT 1),
                (SELECT p.played FROM playback_units p WHERE p.user_id=?1 AND p.media_id=?2 AND p.season=o.season AND p.episode=o.episode),0) AS played FROM owned o)
                SELECT season,COUNT(*),SUM(played) FROM marks GROUP BY season ORDER BY season";
            Ok(tx.prepare_cached(sql)?.query_map(params![user.to_string(),media.to_string(),ids],|r|Ok(ProtocolSeason{season:r.get(0)?,total:r.get(1)?,played:r.get(2)?}))?.collect::<Result<_,_>>()?)
        })
    }
}
