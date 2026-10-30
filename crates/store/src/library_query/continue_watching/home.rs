//! Home activity and subscription windows keep distinct selection contracts.
use super::{HomeEntry, HomeQuery, HomeWindow, Store, StoreError, read_rows, scope, sql};
use domain::{LibraryId, MediaId, ProtocolItemRef, UserId};

impl Store {
    pub fn home_activity_page(&self, query: &HomeQuery) -> Result<Vec<HomeEntry>, StoreError> {
        let libraries = scope::authorized(self, query.user_id, &query.libraries)?;
        self.with_read(crate::Db::Library, |conn| {
            scope::ready(conn, &libraries)?;
            let query = activity_query(query, &libraries)?;
            Ok(read_rows(conn, &query, true)?
                .into_iter()
                .map(|row| HomeEntry {
                    reference: row.reference,
                    last_activity: row.activity,
                    advanced: row.advanced,
                })
                .collect())
        })
    }

    pub fn home_subscription_page(
        &self,
        user_id: UserId,
        libraries: &[LibraryId],
        windows: &[HomeWindow],
        exclude_media: &[MediaId],
        limit: usize,
    ) -> Result<Vec<HomeEntry>, StoreError> {
        let libraries = scope::authorized(self, user_id, libraries)?;
        self.with_read(crate::Db::Library, |conn| {
            scope::ready(conn, &libraries)?;
            let query = subscription_query(user_id, &libraries, windows, exclude_media, limit)?;
            Ok(read_rows(conn, &query, true)?
                .into_iter()
                .map(|row| HomeEntry {
                    reference: row.reference,
                    last_activity: row.activity,
                    advanced: row.advanced,
                })
                .collect())
        })
    }

    /// The current page's artwork preference, independently scoped from its unit source.
    pub fn home_preferred_artwork_refs(
        &self,
        user_id: UserId,
        libraries: &[LibraryId],
        page_media: &[MediaId],
    ) -> Result<Vec<ProtocolItemRef>, StoreError> {
        let libraries = scope::authorized(self, user_id, libraries)?;
        self.with_read(crate::Db::Library, |conn| {
            scope::ready(conn, &libraries)?;
            let query = artwork_query(user_id, &libraries, page_media)?;
            Ok(read_rows(conn, &query, false)?
                .into_iter()
                .map(|row| row.reference)
                .collect())
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn home_continue_query_plans(
        &self,
        query: &HomeQuery,
        windows: &[HomeWindow],
    ) -> Result<Vec<String>, StoreError> {
        let libraries = scope::authorized(self, query.user_id, &query.libraries)?;
        self.with_read(crate::Db::Library, |conn| {
            let activity = activity_query(query, &libraries)?;
            let subscriptions =
                subscription_query(query.user_id, &libraries, windows, &[], query.limit)?;
            let artwork = artwork_query(
                query.user_id,
                &libraries,
                &windows
                    .iter()
                    .map(|window| window.media_id)
                    .collect::<Vec<_>>(),
            )?;
            super::explain(
                conn,
                &[
                    ("activity", &activity),
                    ("subscriptions", &subscriptions),
                    ("artwork", &artwork),
                ],
            )
        })
    }
}

fn activity_query(query: &HomeQuery, libraries: &[LibraryId]) -> Result<sql::Query, StoreError> {
    let mut scope = scope::Sql::new(query.user_id, libraries, true)?;
    let limit = scope.bind(query.limit.min(100) as i64);
    let source = sql::source_sql("p", None);
    Ok(sql::Query {
        sql: format!(
            "WITH {}, candidates AS (SELECT u.library_id,u.media_id,CASE WHEN i.kind='tv' THEN u.activity_next_season ELSE u.resume_season END AS season,CASE WHEN i.kind='tv' THEN u.activity_next_episode ELSE u.resume_episode END AS episode,u.activity_at AS activity,CASE WHEN i.kind='tv' AND u.activity_season IS NOT NULL THEN (u.activity_next_season<>u.activity_season OR u.activity_next_episode<>u.activity_episode) ELSE 0 END AS advanced FROM user_library_items u INDEXED BY user_items_home JOIN library_items i ON i.library_id=u.library_id AND i.media_id=u.media_id WHERE u.user_id=?1 AND u.activity_present=1 AND (u.activity_next_season IS NOT NULL OR u.resume_season IS NOT NULL) AND {} AND ((i.kind='tv' AND u.activity_next_season IS NOT NULL) OR (i.kind<>'tv' AND u.resume_season IS NOT NULL))), ranked AS (SELECT c.*,ROW_NUMBER() OVER(PARTITION BY media_id ORDER BY season,episode,library_id) AS position FROM candidates c), page AS MATERIALIZED (SELECT * FROM ranked WHERE position=1 ORDER BY activity DESC,media_id LIMIT {limit}) SELECT p.library_id,p.media_id,p.season,p.episode,p.activity,f.file_id,f.file_revision,p.advanced FROM page p JOIN library_files f ON f.file_id=({source}) ORDER BY p.activity DESC,p.media_id",
            scope.allowed, scope.cards
        ),
        values: scope.values,
    })
}

fn subscription_query(
    user_id: UserId,
    libraries: &[LibraryId],
    windows: &[HomeWindow],
    exclude_media: &[MediaId],
    limit: usize,
) -> Result<sql::Query, StoreError> {
    let mut scope = scope::Sql::new(user_id, libraries, true)?;
    let windows: Vec<_> = windows.iter().map(|window| serde_json::json!({"media":window.media_id,"season":window.season,"start":window.episode_from,"end":window.episode_to})).collect();
    let windows = scope.bind(serde_json::to_string(&windows)?);
    let exclude = scope.bind(serde_json::to_string(exclude_media)?);
    let cap = scope.bind(i64::from(domain::Coverage::MAX_EPISODES) - 1);
    let limit = scope.bind(limit.min(100) as i64);
    let bounds = format!(
        "unit.season=w.season AND unit.episode>=w.start AND unit.episode<=w.start+{cap} AND (w.finish IS NULL OR unit.episode<=w.finish)"
    );
    let source = sql::source_sql("p", None);
    Ok(sql::Query {
        sql: format!(
            "WITH {}, windows AS (SELECT CAST(j.key AS INTEGER) AS ordinal,json_extract(j.value,'$.media') AS media_id,json_extract(j.value,'$.season') AS season,json_extract(j.value,'$.start') AS start,json_extract(j.value,'$.end') AS finish FROM json_each({windows}) j), owners AS (SELECT w.ordinal,w.media_id,w.season,i.library_id,COALESCE(u.all_watch_at,0) AS activity,CASE WHEN u.user_id IS NOT NULL THEN (SELECT unit.episode FROM user_library_continue_units unit WHERE unit.user_id=?1 AND unit.library_id=i.library_id AND unit.media_id=i.media_id AND {bounds} ORDER BY unit.episode LIMIT 1) ELSE (SELECT unit.episode FROM library_units unit WHERE unit.library_id=i.library_id AND unit.media_id=i.media_id AND unit.available_file_count>0 AND {bounds} ORDER BY unit.episode LIMIT 1) END AS episode,EXISTS(SELECT 1 FROM playback_units raw INDEXED BY playback_played_episodes WHERE raw.user_id=?1 AND raw.media_id=i.media_id AND raw.season=w.season AND raw.played<>0 AND raw.season>=0 AND raw.episode>=0) AS advanced FROM windows w JOIN library_items i ON i.media_id=w.media_id LEFT JOIN user_library_items u ON u.user_id=?1 AND u.library_id=i.library_id AND u.media_id=i.media_id WHERE i.kind='tv' AND {} AND i.media_id NOT IN (SELECT value FROM json_each({exclude}))), ranked AS (SELECT o.*,ROW_NUMBER() OVER(PARTITION BY media_id ORDER BY ordinal,episode,library_id) AS position FROM owners o WHERE episode IS NOT NULL), page AS MATERIALIZED (SELECT * FROM ranked WHERE position=1 ORDER BY ordinal,media_id LIMIT {limit}) SELECT p.library_id,p.media_id,p.season,p.episode,p.activity,f.file_id,f.file_revision,p.advanced FROM page p JOIN library_files f ON f.file_id=({source}) ORDER BY p.ordinal,p.media_id",
            scope.allowed, scope.cards
        ),
        values: scope.values,
    })
}

fn artwork_query(
    user_id: UserId,
    libraries: &[LibraryId],
    media: &[MediaId],
) -> Result<sql::Query, StoreError> {
    let mut scope = scope::Sql::new(user_id, libraries, true)?;
    let media = scope.bind(serde_json::to_string(media)?);
    Ok(sql::Query {
        sql: format!(
            "WITH {}, ranked AS (SELECT f.library_id,f.media_id,fu.season,fu.episode,f.file_id,f.file_revision,ROW_NUMBER() OVER(PARTITION BY f.media_id ORDER BY MAX(fu.season,0) DESC,MAX(fu.episode,0) DESC,f.display_path DESC,f.file_id) AS position FROM json_each({media}) wanted CROSS JOIN library_files f ON f.media_id=wanted.value JOIN library_file_units fu ON fu.file_id=f.file_id WHERE f.library_id IN(SELECT library_id FROM allowed) AND f.publication_state='published' AND f.presence_state='present') SELECT library_id,media_id,season,episode,0,file_id,file_revision FROM ranked WHERE position=1 ORDER BY media_id",
            scope.allowed
        ),
        values: scope.values,
    })
}
