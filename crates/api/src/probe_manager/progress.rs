use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::fingerprint_job::CaptureGate;
use super::worker::wait_for_metadata_idle;
use super::ProbeManager;

pub struct MetadataPriorityGate {
    manager: Arc<ProbeManager>,
}

impl MetadataPriorityGate {
    pub fn new(manager: Arc<ProbeManager>) -> Self {
        Self { manager }
    }
}

impl CaptureGate for MetadataPriorityGate {
    fn wait_before_capture(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            wait_for_metadata_idle(&self.manager).await;
        })
    }
}
