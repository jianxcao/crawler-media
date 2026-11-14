use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

/// Hold the selected production writer until the test releases it.
/// The test thread can still drive requests while the SQL seam is occupied.
pub struct WriterHold {
    release: Option<mpsc::Sender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl WriterHold {
    pub fn acquire(store: Arc<store::Store>, db: store::Db) -> Self {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            store
                .with_writer(db, |_| {
                    ready_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                })
                .unwrap();
        });
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        Self {
            release: Some(release_tx),
            worker: Some(worker),
        }
    }
}

impl Drop for WriterHold {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}
