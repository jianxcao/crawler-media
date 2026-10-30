//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn library_facets(&self, request: &FacetRequest) -> Result<LibraryFacets, StoreError> {
        self.library_facets_inner(request)
    }

    pub(super) fn library_facets_inner(
        &self,
        request: &FacetRequest,
    ) -> Result<LibraryFacets, StoreError> {
        let tx = crate::read_tx(self.library)?;
        let result = library_facets_on(&tx, request)?;
        tx.commit()?;
        Ok(result)
    }
}
