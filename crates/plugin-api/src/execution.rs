use std::{cell::Cell, marker::PhantomData, rc::Rc};

thread_local! { static IN_GUEST: Cell<bool> = const { Cell::new(false) }; }

/// Marks a worker thread while it executes a guest and its host imports.
pub struct GuestExecutionGuard {
    previous: bool,
    _same_thread: PhantomData<Rc<()>>,
}

impl GuestExecutionGuard {
    pub fn enter() -> Self {
        Self {
            previous: IN_GUEST.with(|active| active.replace(true)),
            _same_thread: PhantomData,
        }
    }
}

impl Drop for GuestExecutionGuard {
    fn drop(&mut self) {
        IN_GUEST.with(|active| active.set(self.previous));
    }
}

pub fn in_guest_execution() -> bool {
    IN_GUEST.with(Cell::get)
}
