use std::sync::{Arc, Condvar, Mutex};

/// A blocking test gate whose guard releases waiters on every exit path.
pub struct Gate {
    state: Arc<(Mutex<bool>, Condvar)>,
}

impl Gate {
    pub fn new() -> Self {
        Self {
            state: Arc::new((Mutex::new(false), Condvar::new())),
        }
    }

    pub fn wait(&self) {
        let (lock, cvar) = &*self.state;
        let released = *cvar
            .wait_timeout_while(
                lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner()),
                std::time::Duration::from_secs(30),
                |released| !*released,
            )
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .0;
        assert!(released, "test must release the remediation gate");
    }

    /// Another handle to the same gate (released by the same guard).
    pub fn clone_handle(&self) -> Self {
        Self {
            state: self.state.clone(),
        }
    }

    pub fn release_guard(&self) -> GateRelease {
        GateRelease(self.state.clone())
    }
}

pub struct GateRelease(Arc<(Mutex<bool>, Condvar)>);

impl Drop for GateRelease {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.0;
        *lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        cvar.notify_all();
    }
}

/// Shared temp-backed API fixture; all external fetches are disabled by the existing fixture.
pub fn fixture() -> crate::support::library_presence::PresenceFixture {
    crate::support::library_presence::PresenceFixture::new()
}
