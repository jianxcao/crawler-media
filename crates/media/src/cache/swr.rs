//! Thread-scoped Stale-While-Revalidate (SWR) mode for Discover/list reads.
use std::cell::RefCell;

pub struct PreferStaleOutcome<T> {
    pub value: T,
    pub stale_keys: Vec<String>,
}

impl<T> PreferStaleOutcome<T> {
    pub fn used_stale(&self) -> bool {
        !self.stale_keys.is_empty()
    }
}

thread_local! {
    static STALE_KEYS: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

struct Restore(Option<Vec<String>>);

impl Drop for Restore {
    fn drop(&mut self) {
        STALE_KEYS.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}

/// Run `work` in Stale-While-Revalidate mode: expired but valid discovery/list
/// cache entries return immediately without waiting on upstream, and their
/// cache keys are recorded in [`PreferStaleOutcome::stale_keys`] so the caller
/// can trigger an asynchronous background refresh.
pub fn with_prefer_stale<T>(work: impl FnOnce() -> T) -> PreferStaleOutcome<T> {
    let previous = STALE_KEYS.with(|slot| slot.replace(Some(Vec::new())));
    let mut restore = Restore(previous);
    let value = work();
    let stale_keys = STALE_KEYS
        .with(|slot| slot.replace(restore.0.take()))
        .unwrap_or_default();
    PreferStaleOutcome { value, stale_keys }
}

pub(super) fn is_prefer_stale() -> bool {
    STALE_KEYS.with(|slot| slot.borrow().is_some())
}

pub(super) fn record_stale_key(key: &str) {
    STALE_KEYS.with(|slot| {
        if let Some(keys) = slot.borrow_mut().as_mut()
            && !keys.iter().any(|existing| existing == key)
        {
            keys.push(key.to_string());
        }
    });
}
