use api::ApiState;
use std::sync::mpsc;
use std::time::Duration;

/// Hold the real app connection without holding the outer Store mutex.
pub struct HeldAppWriter {
    release: Option<mpsc::Sender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl HeldAppWriter {
    pub fn acquire(state: &ApiState) -> Self {
        let writers = state.store().lock().writers();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            writers
                .with_writer(store::Db::App, |_| {
                    entered_tx.send(()).unwrap();
                    // Disconnection also releases this lease during unwinding.
                    let _ = release_rx.recv_timeout(Duration::from_secs(5));
                    Ok(())
                })
                .unwrap();
        });
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        Self {
            release: Some(release_tx),
            worker: Some(worker),
        }
    }
}

impl Drop for HeldAppWriter {
    fn drop(&mut self) {
        self.release.take();
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}
