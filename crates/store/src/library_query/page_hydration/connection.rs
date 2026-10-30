//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn page_hydration(
        &self,
        files: &[LedgerId],
    ) -> Result<HashMap<LedgerId, HydratedFile>, StoreError> {
        let tx = crate::read_tx(self.library)?;
        let result = hydrate_on(&tx, files)?;
        tx.commit()?;
        Ok(result)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn page_read_query_plans(&self) -> Result<Vec<String>, StoreError> {
        let mut result = Vec::new();
        for sql in super::super::page_validate::VALIDATION_PLANS
            .iter()
            .copied()
            .chain([OWNERS_SQL, DETAILS_SQL, UNITS_SQL, MARKERS_SQL])
            .chain(metadata::PLANS.iter().copied())
        {
            let mut stmt = self.library.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
            let rows = stmt.query_map(["[]"], |r| r.get::<_, String>(3))?;
            result.extend(rows.collect::<Result<Vec<_>, _>>()?);
        }
        Ok(result)
    }
}
