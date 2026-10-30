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
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let store = crate::Store::open(dir.path().join("data")).unwrap();
        store
            .configure_reader_pool_size(store::Db::App, std::num::NonZeroUsize::new(1).unwrap())
            .unwrap();
        store
            .with_read(store::Db::App, |connection| {
                connection.authorizer(Some(|context: AuthContext<'_>| match context.action {
                    AuthAction::Read {
                        table_name: "users",
                        ..
                    } => Authorization::Deny,
                    _ => Authorization::Allow,
                }));
                Ok(())
            })
            .unwrap();
        let error =
            super::super::scope::request_scope(&store, UserId::new(), &ServerItemQuery::default())
                .err()
                .unwrap();
        store
            .with_read(store::Db::App, |connection| {
                connection.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
                Ok(())
            })
            .unwrap();
        assert!(matches!(error, store::StoreError::Sqlite(_)), "{error}");
    }
}
