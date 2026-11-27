//! Shared cancellation signal for media child processes; no API dependencies.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Debug)]
pub struct WorkControl {
    cancelled: Arc<AtomicBool>,
    ancestors: Vec<Arc<AtomicBool>>,
    publications: Arc<[Arc<Mutex<()>>]>,
    read_stops: Arc<[Arc<AtomicBool>]>,
}

impl Default for WorkControl {
    fn default() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            ancestors: Vec::new(),
            publications: Arc::from([Arc::new(Mutex::new(()))]),
            read_stops: Arc::from([]),
        }
    }
}

fn merge_sorted_arcs<T>(first: &[Arc<T>], second: &[Arc<T>]) -> Arc<[Arc<T>]> {
    let mut merged: Vec<Arc<T>> = first.iter().chain(second.iter()).cloned().collect();
    merged.sort_by_key(|item| Arc::as_ptr(item) as usize);
    merged.dedup_by(|a, b| Arc::ptr_eq(a, b));
    Arc::from(merged)
}

impl WorkControl {
    /// Create a child signal that observes parent cancellation while allowing
    /// independent child-scoped cancellation without cancelling parent leases.
    pub fn linked_child(&self) -> Self {
        let mut ancestors = Vec::with_capacity(self.ancestors.len() + 1);
        ancestors.extend(self.ancestors.iter().cloned());
        if !ancestors
            .iter()
            .any(|flag| Arc::ptr_eq(flag, &self.cancelled))
        {
            ancestors.push(self.cancelled.clone());
        }
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            ancestors,
            publications: self.publications.clone(),
            read_stops: self.read_stops.clone(),
        }
    }

    /// Combine another cancellation signal so cancelling either aborts this child.
    pub fn with_linked_abort(&self, other: &Self) -> Self {
        let mut child = self.linked_child();
        for flag in other
            .ancestors
            .iter()
            .chain(std::iter::once(&other.cancelled))
        {
            if !child
                .ancestors
                .iter()
                .any(|existing| Arc::ptr_eq(existing, flag))
            {
                child.ancestors.push(flag.clone());
            }
        }
        child.publications = merge_sorted_arcs(&self.publications, &other.publications);
        child.read_stops = merge_sorted_arcs(&self.read_stops, &other.read_stops);
        child
    }

    /// Stop waiting for admission without interrupting an admitted read.
    pub fn with_read_stop(mut self, stop: Arc<AtomicBool>) -> Self {
        self.read_stops = merge_sorted_arcs(&self.read_stops, &[stop]);
        self
    }

    pub fn should_stop_read_wait(&self) -> bool {
        self.is_cancelled() || self.read_wait_stopped()
    }

    pub(crate) fn read_wait_stopped(&self) -> bool {
        self.read_stops
            .iter()
            .any(|stop| stop.load(Ordering::Acquire))
    }

    pub fn cancel(&self) {
        let _guards: Vec<_> = self
            .publications
            .iter()
            .map(|gate| gate.lock().unwrap_or_else(|poison| poison.into_inner()))
            .collect();
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn publish<T>(&self, publish: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        let mut guards = Vec::with_capacity(self.publications.len());
        for gate in self.publications.iter() {
            guards.push(
                gate.lock()
                    .map_err(|_| "publication gate poisoned".to_string())?,
            );
        }
        if self.is_cancelled() {
            return Err("Library work cancelled".into());
        }
        let result = publish();
        drop(guards);
        result
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
            || self
                .ancestors
                .iter()
                .any(|flag| flag.load(Ordering::Acquire))
    }
    pub fn output(
        &self,
        command: &mut std::process::Command,
    ) -> Result<std::process::Output, String> {
        use std::{io::Read, process::Stdio, thread, time::Duration};
        if self.is_cancelled() {
            return Err("Library work cancelled".into());
        }
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let out = thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = stdout {
                pipe.read_to_end(&mut bytes)?;
            }
            Ok::<_, std::io::Error>(bytes)
        });
        let err = thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = stderr {
                pipe.read_to_end(&mut bytes)?;
            }
            Ok::<_, std::io::Error>(bytes)
        });
        let status = loop {
            if self.is_cancelled() {
                let _ = child.kill();
                break child.wait();
            }
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => thread::sleep(Duration::from_millis(25)),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(error);
                }
            }
        };
        let stdout = out
            .join()
            .map_err(|_| "stdout reader panicked")?
            .map_err(|e| e.to_string())?;
        let stderr = err
            .join()
            .map_err(|_| "stderr reader panicked")?
            .map_err(|e| e.to_string())?;
        if self.is_cancelled() {
            return Err("Library work cancelled".into());
        }
        Ok(std::process::Output {
            status: status.map_err(|e| e.to_string())?,
            stdout,
            stderr,
        })
    }
}
