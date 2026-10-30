//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn library_item_page(&self, query: &LibraryQuery) -> Result<ItemPage, StoreError> {
        self.library_item_page_inner(query).map(|(page, _)| page)
    }

    pub(super) fn library_item_page_inner(
        &self,
        query: &LibraryQuery,
    ) -> Result<(ItemPage, super::super::stats::QueryWorkStats), StoreError> {
        let tx = crate::read_tx(self.library)?;
        let result = library_item_page_on(&tx, query)?;
        tx.commit()?;
        Ok(result)
    }

    pub fn library_item_titles(&self, query: &LibraryQuery) -> Result<Vec<String>, StoreError> {
        self.ensure_library_query_ready(query.library_id)?;
        let base = ItemQuerySql::build_for_order(
            query.library_id,
            &query.filter,
            query.user_id,
            matches!(query.order, BrowseOrder::LastPlayed) || query.filter.unwatched_first,
        )?;
        let sql = format!(
            "{} ORDER BY {}",
            base.select("i.title"),
            order_by_sql(&order_spec(query))
        );
        let mut statement = self.library.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(base.bindings.iter().cloned()), |row| {
            row.get::<_, String>(0)
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn library_item_index(&self, query: &LibraryQuery) -> Result<Vec<IndexBucket>, StoreError> {
        library_item_index_on(self.library, query)
    }

    pub fn library_item_count(&self, query: &LibraryQuery) -> Result<u64, StoreError> {
        let tx = crate::read_tx(self.library)?;
        let count = library_item_count_on(&tx, query)?;
        tx.commit()?;
        Ok(count)
    }
}
