use marker::{
    CaptureFailure, CaptureRequest, CaptureSourceFacts, CapturedFingerprint,
    FingerprintCaptureEngine, WorkControl,
};
use std::sync::Arc;

pub(crate) struct CaptureMeasurement {
    pub engine: Arc<dyn FingerprintCaptureEngine>,
    pub timings: crate::probe_manager::timings::ProbeTimingLedger,
    pub job_id: Option<String>,
}
impl FingerprintCaptureEngine for CaptureMeasurement {
    fn capture_window_after_metadata_controlled(
        &self,
        request: &CaptureRequest,
        source: Option<&CaptureSourceFacts>,
        control: &WorkControl,
    ) -> Result<CapturedFingerprint, CaptureFailure> {
        let result = self
            .engine
            .capture_window_after_metadata_controlled(request, source, control);
        self.timings.record_capture(self.job_id.as_deref(), &result);
        result
    }
    fn capture_window(
        &self,
        request: &CaptureRequest,
    ) -> Result<CapturedFingerprint, CaptureFailure> {
        self.capture_window_controlled(request, &WorkControl::default())
    }
    fn capture_window_controlled(
        &self,
        request: &CaptureRequest,
        control: &WorkControl,
    ) -> Result<CapturedFingerprint, CaptureFailure> {
        let result = self.engine.capture_window_controlled(request, control);
        self.timings.record_capture(self.job_id.as_deref(), &result);
        result
    }
    fn capture_window_from_source_controlled(
        &self,
        request: &CaptureRequest,
        source: &CaptureSourceFacts,
        control: &WorkControl,
    ) -> Result<CapturedFingerprint, CaptureFailure> {
        let result = self
            .engine
            .capture_window_from_source_controlled(request, source, control);
        self.timings.record_capture(self.job_id.as_deref(), &result);
        result
    }
}
