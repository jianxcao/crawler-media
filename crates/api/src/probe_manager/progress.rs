use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

use crate::fingerprint_job::CaptureGate;
use super::ProbeManager;

#[derive(Clone)]
pub struct MetadataPriorityGate {
    metadata_pending: Arc<AtomicUsize>,
    metadata_idle: Arc<Notify>,
}

impl MetadataPriorityGate {
    pub fn new(manager: Arc<ProbeManager>) -> Self {
        Self {
            metadata_pending: manager.metadata_pending.clone(),
            metadata_idle: manager.metadata_idle.clone(),
        }
    }

    pub fn from_manager(manager: &ProbeManager) -> Self {
        Self {
            metadata_pending: manager.metadata_pending.clone(),
            metadata_idle: manager.metadata_idle.clone(),
        }
    }
}

impl CaptureGate for MetadataPriorityGate {
    fn wait_before_capture(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            loop {
                let notified = self.metadata_idle.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self.metadata_pending.load(Ordering::Acquire) == 0 {
                    return;
                }
                notified.await;
            }
        })
    }
}
