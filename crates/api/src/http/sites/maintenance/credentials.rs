//! Credentials are published only while the captured Site and write admission remain current.
use std::cell::RefCell;

use axum::response::Response;
use domain::{Site, SiteId};
use hooks::{PluginError, SiteCredentials};

use crate::ApiState;
use crate::http::store_wait::{STORE_READ_WAIT, busy_response, with_foreground_store};

pub(super) struct AdmittedCredentials<'a> {
    state: &'a ApiState,
    expected: Site,
    rejected: RefCell<Option<Response>>,
}
impl<'a> AdmittedCredentials<'a> {
    pub(super) fn new(state: &'a ApiState, expected: Site) -> Self {
        Self {
            state,
            expected,
            rejected: RefCell::new(None),
        }
    }
    pub(super) fn take_rejection(&self) -> Option<Response> {
        self.rejected.take()
    }
    fn with_store<T>(
        &self,
        work: impl FnOnce(&crate::Store) -> Result<T, PluginError>,
    ) -> Result<T, PluginError> {
        with_foreground_store(self.state, work).map_err(|response| self.refused(response))?
    }
    fn refused(&self, response: Response) -> PluginError {
        self.rejected.replace(Some(response));
        tracing::warn!(site_id = %self.expected.id, "Site credential publication rejected by write admission");
        PluginError::Fetch("Site maintenance write admission changed".into())
    }
    fn unchanged_site(&self, store: &crate::Store) -> Result<Site, PluginError> {
        let current = store
            .get_site(self.expected.id)
            .map_err(|error| PluginError::Fetch(error.to_string()))?
            .ok_or(PluginError::NotFound)?;
        if current != self.expected {
            tracing::warn!(site_id = %self.expected.id, "Site changed during credential maintenance; stale result discarded");
            return Err(PluginError::Fetch("Site changed during maintenance".into()));
        }
        Ok(current)
    }
}
impl SiteCredentials for AdmittedCredentials<'_> {
    fn load(&self, id: SiteId) -> Result<Site, PluginError> {
        if id != self.expected.id {
            return Err(PluginError::NotFound);
        }
        self.state
            .require_writable()
            .and_then(|_| self.state.require_write_admitted())
            .map_err(|response| self.refused(response))?;
        let reader = self
            .state
            .store_readers
            .checkout(STORE_READ_WAIT)
            .ok_or_else(|| self.refused(busy_response()))?;
        self.unchanged_site(&reader)
    }
    fn save(&self, site: &Site) -> Result<(), PluginError> {
        if site.id != self.expected.id {
            return Err(PluginError::NotFound);
        }
        self.with_store(|store| {
            match store
                .save_site_if_current(site, &self.expected)
                .map_err(|error| PluginError::Fetch(error.to_string()))?
            {
                true => Ok(()),
                false => {
                    tracing::warn!(site_id = %site.id, "Stale Site credential result was rejected atomically");
                    Err(PluginError::Fetch("Site changed during maintenance".into()))
                }
            }
        })
    }
}

#[cfg(test)]
mod tests;
