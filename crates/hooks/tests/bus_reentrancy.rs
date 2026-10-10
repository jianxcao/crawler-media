use std::sync::{Arc, mpsc};
use std::time::Duration;

use hooks::{Bus, Hook, HookEvent, Step};

#[test]
fn hook_can_register_another_hook_and_emit_another_step() {
    let bus = Arc::new(Bus::new());
    let weak = Arc::downgrade(&bus);
    let (tx, rx) = mpsc::channel();
    bus.register(Hook::new(Step::Transfer, move |_| {
        let bus = weak.upgrade().unwrap();
        let tx = tx.clone();
        bus.register(Hook::new(Step::Scrape, move |_| {
            tx.send(()).unwrap();
            Ok(())
        }));
        bus.emit(&HookEvent { step: Step::Scrape })
    }));
    std::thread::spawn(move || {
        bus.emit(&HookEvent {
            step: Step::Transfer,
        })
        .unwrap()
    });
    rx.recv_timeout(Duration::from_secs(3))
        .expect("Hook execution must not retain the registry mutex");
}
