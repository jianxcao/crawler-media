//! Collection cursors bind both ordering and visibility scope.
use domain::{LibraryId, UserId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::StoreError;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Cursor {
    scope: String,
    pub order: i64,
    pub added: String,
    pub media: String,
}

pub(super) fn scope(user: UserId, collection: &str, libraries: &[LibraryId]) -> String {
    let mut libraries: Vec<_> = libraries.iter().map(ToString::to_string).collect();
    libraries.sort();
    libraries.dedup();
    let mut hash = Sha256::new();
    hash.update(user.to_string());
    hash.update([0]);
    hash.update(collection);
    for library in libraries {
        hash.update([0]);
        hash.update(library);
    }
    format!("{:x}", hash.finalize())
}

pub(super) fn decode(raw: Option<&str>, scope: &str) -> Result<Option<Cursor>, StoreError> {
    raw.map(|raw| {
        let cursor: Cursor = serde_json::from_str(raw)
            .map_err(|_| StoreError::Invalid("invalid collection cursor".into()))?;
        if cursor.scope != scope || cursor.media.parse::<domain::MediaId>().is_err() {
            return Err(StoreError::Invalid(
                "collection cursor scope mismatch".into(),
            ));
        }
        Ok(cursor)
    })
    .transpose()
}

pub(super) fn encode(
    scope: String,
    order: i64,
    added: String,
    media: String,
) -> Result<String, StoreError> {
    Ok(serde_json::to_string(&Cursor {
        scope,
        order,
        added,
        media,
    })?)
}
