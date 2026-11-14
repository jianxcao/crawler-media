use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Hold a real cache read on an unlinked inode while later requests see a miss.
pub struct CacheReadPause {
    release: Option<mpsc::Sender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl CacheReadPause {
    pub fn new(path: &Path) -> (Self, tokio::sync::oneshot::Receiver<()>) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: name is a NUL-terminated path owned for the duration of mkfifo.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let path = path.to_owned();
        let (release, released) = mpsc::channel();
        let (entered, opened) = tokio::sync::oneshot::channel();
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            let writer = loop {
                match std::fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&path)
                {
                    Ok(writer) => break writer,
                    Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                        assert!(Instant::now() < deadline, "cache reader never opened FIFO");
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("FIFO writer failed: {error}"),
                }
            };
            // The reader has opened this inode. Removing the name makes a later
            // cache lookup miss without releasing the earlier blocked read.
            std::fs::remove_file(path).unwrap();
            let _ = entered.send(());
            let _ = released.recv_timeout(Duration::from_secs(3));
            drop(writer);
        });
        (
            Self {
                release: Some(release),
                worker: Some(worker),
            },
            opened,
        )
    }
    pub fn finish(mut self) {
        self.release.take().unwrap().send(()).unwrap();
        self.worker.take().unwrap().join().unwrap();
    }
}
impl Drop for CacheReadPause {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}
