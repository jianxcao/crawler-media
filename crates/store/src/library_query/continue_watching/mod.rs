//! Logical continuation pages use durable projections before page hydration.
mod candidates;
mod home;
mod ordered;
mod scope;
mod sql;
mod types;
pub use types::{ContinueEntry, ContinuePage, ContinueQuery, HomeEntry, HomeQuery, HomeWindow};

use crate::{Store, StoreError};
use domain::{PhysicalSourceId, ProtocolItemRef, TypedItemId, UnitKey};
use rusqlite::{Connection, params_from_iter};

impl Store {
    pub fn next_up_page(&self, query: &ContinueQuery) -> Result<ContinuePage, StoreError> {
        self.continue_page(query, sql::Purpose::Next)
    }
    pub fn resume_page(&self, query: &ContinueQuery) -> Result<ContinuePage, StoreError> {
        self.continue_page(query, sql::Purpose::Resume)
    }
    fn continue_page(
        &self,
        query: &ContinueQuery,
        purpose: sql::Purpose,
    ) -> Result<ContinuePage, StoreError> {
        let libraries = scope::authorized(self, query.user_id, &query.libraries)?;
        self.with_read(crate::Db::Library, |conn| {
            scope::ready(conn, &libraries)?;
            let queries = sql::queries(query, &libraries, purpose)?;
            let total = ordered::count(conn, &queries)?;
            let items = ordered::page(conn, &queries, query)?;
            Ok(ContinuePage { items, total })
        })
    }

    /// Explain both actual count and page statements with the production bindings.
    #[cfg(any(test, feature = "test-support"))]
    pub fn continue_watching_query_plans(
        &self,
        query: &ContinueQuery,
    ) -> Result<(Vec<String>, Vec<String>), StoreError> {
        let libraries = scope::authorized(self, query.user_id, &query.libraries)?;
        self.with_read(crate::Db::Library, |conn| {
            let next = sql::queries(query, &libraries, sql::Purpose::Next)?;
            let resume = sql::queries(query, &libraries, sql::Purpose::Resume)?;
            Ok((
                ordered::explain(conn, &next, query)?,
                ordered::explain(conn, &resume, query)?,
            ))
        })
    }
}

struct Selected {
    reference: ProtocolItemRef,
    activity: i64,
    advanced: bool,
}
fn read_rows(
    conn: &Connection,
    query: &sql::Query,
    home: bool,
) -> Result<Vec<Selected>, StoreError> {
    let mut statement = conn.prepare_cached(&query.sql)?;
    let rows = statement.query_map(params_from_iter(&query.values), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i32>(2)?,
            row.get::<_, i32>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, i64>(6)?,
            if home { row.get::<_, bool>(7)? } else { false },
        ))
    })?;
    rows.map(|row| {
        let (library, media, season, episode, activity, file, revision, advanced) = row?;
        let media = media.parse()?;
        let item = if (season, episode) == (-1, -1) {
            TypedItemId::Work(media)
        } else {
            TypedItemId::Unit {
                media,
                key: UnitKey::new(season, episode),
            }
        };
        Ok(Selected {
            reference: ProtocolItemRef {
                library_id: library.parse()?,
                item,
                preferred_source: Some(PhysicalSourceId {
                    file_id: file.parse()?,
                    revision,
                }),
            },
            activity,
            advanced,
        })
    })
    .collect()
}
#[cfg(any(test, feature = "test-support"))]
fn explain(conn: &Connection, queries: &[(&str, &sql::Query)]) -> Result<Vec<String>, StoreError> {
    let mut result = Vec::new();
    for (phase, query) in queries {
        let mut statement = conn.prepare(&format!("EXPLAIN QUERY PLAN {}", query.sql))?;
        for row in statement.query_map(params_from_iter(&query.values), |row| {
            row.get::<_, String>(3)
        })? {
            result.push(format!("{phase}: {}", row?));
        }
    }
    Ok(result)
}
