//! Lightweight protocol request classification before SQL page selection.

use domain::{ProtocolItemQuery, UserId};
use media_server::provider::{QueryError, ServerItemQuery};

pub(crate) fn resolve_page_request(
    store: &crate::Store,
    user: UserId,
    query: &ServerItemQuery,
) -> Result<Option<ProtocolItemQuery>, QueryError> {
    let scope =
        super::scope::request_scope(store, user, query).map_err(super::query_store_error)?;
    if scope.unresolved {
        return Ok(None);
    }
    super::page::resolve_query(store, &scope, query)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_failure_keeps_typed_sqlite_error_for_query_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::Store::open(dir.path().join("data")).unwrap();
        let conn = rusqlite::Connection::open(dir.path().join("data/app.db")).unwrap();
        conn.execute_batch("DROP TABLE users").unwrap();
        let error =
            super::super::scope::request_scope(&store, UserId::new(), &ServerItemQuery::default())
                .err()
                .unwrap();
        assert!(matches!(error, store::StoreError::Sqlite(_)));
    }
}
