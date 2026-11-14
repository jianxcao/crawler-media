//! Same-path drains must not overlap: the first worker may complete its intent
//! before a second worker reaches unlink. Hold this claim through the final SQL.
//! Lock order is path claim -> Store; the registry is never held while waiting.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, Weak};

use parking_lot::Mutex;

pub(crate) fn path_claim(path_key: &str) -> Arc<Mutex<()>> {
    static CLAIMS: OnceLock<Mutex<HashMap<String, Weak<Mutex<()>>>>> = OnceLock::new();
    let mut claims = CLAIMS.get_or_init(Mutex::default).lock();
    claims.retain(|_, claim| claim.strong_count() != 0);
    if let Some(claim) = claims.get(path_key).and_then(Weak::upgrade) {
        return claim;
    }
    let claim = Arc::new(Mutex::new(()));
    claims.insert(path_key.to_string(), Arc::downgrade(&claim));
    claim
}
