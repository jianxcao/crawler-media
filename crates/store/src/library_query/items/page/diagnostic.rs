use super::{
    BrowseOrder, BrowsePage, ItemQuerySql, LibraryQuery, Store, StoreError,
    ensure_library_query_ready_on, order_spec, page_statement, params_from_iter,
};

impl Store {
    /// Explain the production title-ordered first-page statement with its bindings.
    pub fn library_item_page_query_plans(
        &self,
        query: &LibraryQuery,
    ) -> Result<Vec<String>, StoreError> {
        if !matches!(query.order, BrowseOrder::Title)
            || !matches!(query.page, BrowsePage::Cursor { after: None, .. })
            || query.include_total
        {
            return Err(StoreError::Invalid(
                "browse plan diagnostic supports title-ordered cursor first pages without totals"
                    .into(),
            ));
        }
        self.with_library_read(|db| {
            let tx = crate::read_tx(db.library)?;
            ensure_library_query_ready_on(&tx, query.library_id)?;
            let base = ItemQuerySql::build_for_order(
                query.library_id,
                &query.filter,
                query.user_id,
                query.filter.unwatched_first,
            )?;
            let (sql, _, bindings) = page_statement(query, &base, &order_spec(query))?;
            let plans = {
                let mut statement = tx.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
                let rows = statement.query_map(params_from_iter(&bindings), |row| row.get(3))?;
                rows.collect::<Result<Vec<String>, _>>()?
            };
            tx.commit()?;
            Ok(plans)
        })
    }
}
