//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub(crate) fn refresh_user_library_summary(
        &self,
        user_id: UserId,
        media_id: MediaId,
        scope: SummaryScope,
    ) -> Result<(), StoreError> {
        let Some(media) = self.get_media(media_id)? else {
            return Ok(());
        };
        let is_tv = media.kind == domain::MediaKind::Tv;
        let libraries = Store::summary_libraries(self.library, media_id, scope, is_tv)?;
        if libraries.is_empty() {
            return Ok(());
        }
        let facts = Store::summary_facts(self.library, user_id, media_id)?;
        for library_id in &libraries {
            Store::write_library_summary(
                self.library,
                user_id,
                media_id,
                library_id,
                is_tv,
                &facts,
            )?;
        }
        Ok(())
    }

    pub fn get_user_library_summary(
        &self,
        user_id: UserId,
        library_id: domain::LibraryId,
        media_id: MediaId,
    ) -> Result<Option<UserLibrarySummary>, StoreError> {
        let sql = "SELECT summary_revision, seen, played, favorite, last_played_at, favorite_at,
                          owned_episode_count, completed_episode_count, next_season, next_episode,
                          resume_season, resume_episode
                   FROM user_library_items
                   WHERE user_id = ?1 AND library_id = ?2 AND media_id = ?3";
        let lib_str = library_id.to_string();
        self.library
            .query_row(
                sql,
                params![user_id.to_string(), lib_str, media_id.to_string()],
                |row| {
                    let s = map_summary(row)?;
                    Ok(UserLibrarySummary {
                        user_id,
                        library_id,
                        media_id,
                        seen: s.seen,
                        played: s.played,
                        favorite: s.favorite,
                        last_played_at: s.last_played_at,
                        favorite_at: s.favorite_at,
                        owned_episode_count: s.owned_episode_count,
                        completed_episode_count: s.completed_episode_count,
                        next_season: s.next_season,
                        next_episode: s.next_episode,
                        resume_season: s.resume_season,
                        resume_episode: s.resume_episode,
                        summary_revision: row.get(0)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn refresh_merged_summaries(
        &self,
        tx: &Connection,
        media_id: MediaId,
    ) -> Result<(), StoreError> {
        let Some(media) = self.get_media(media_id)? else {
            return Ok(());
        };
        let is_tv = media.kind == domain::MediaKind::Tv;
        let users = query_ids(
            tx,
            "SELECT DISTINCT user_id FROM playback_units WHERE media_id = ?1 ORDER BY user_id",
            media_id,
        )?;
        if users.is_empty() {
            return Ok(());
        }
        let libraries = query_ids(
            tx,
            "SELECT DISTINCT library_id FROM library_files WHERE media_id = ?1 ORDER BY library_id",
            media_id,
        )?;
        for user in &users {
            let user_id: UserId = user.parse()?;
            let facts = Store::summary_facts(tx, user_id, media_id)?;
            for library_id in &libraries {
                Store::write_library_summary(tx, user_id, media_id, library_id, is_tv, &facts)?;
            }
        }
        tracing::info!(%media_id, users = users.len(), libraries = libraries.len(), "refreshed merged media summaries");
        Ok(())
    }
}
