use std::fmt;

use crate::Origin;

#[derive(Clone)]
pub struct CredentialSlot {
    pub site_id: String,
    pub bound_origin: Origin,
    pub credential_epoch: String,
    headers: Vec<(String, String)>,
}

impl CredentialSlot {
    pub fn new(
        site_id: impl Into<String>,
        bound_origin: Origin,
        credential_epoch: impl Into<String>,
        headers: Vec<(String, String)>,
    ) -> Self {
        Self {
            site_id: site_id.into(),
            bound_origin,
            credential_epoch: credential_epoch.into(),
            headers,
        }
    }

    pub fn headers_for_origin(&self, origin: &Origin) -> Option<&[(String, String)]> {
        if &self.bound_origin == origin {
            Some(&self.headers)
        } else {
            None
        }
    }
}

impl fmt::Debug for CredentialSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialSlot")
            .field("site_id", &self.site_id)
            .field("bound_origin", &self.bound_origin)
            .field("credential_epoch", &self.credential_epoch)
            .field("headers", &"<redacted>")
            .finish()
    }
}
