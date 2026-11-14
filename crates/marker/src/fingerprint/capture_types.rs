use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SampleWindow {
    pub start_ms: i64,
    pub end_ms: i64,
}

impl SampleWindow {
    pub fn new(start_ms: i64, end_ms: i64) -> Self {
        Self { start_ms, end_ms }
    }

    pub fn duration_ms(&self) -> i64 {
        self.end_ms.saturating_sub(self.start_ms)
    }

    pub fn validate(&self) -> Result<(), CaptureFailure> {
        if self.start_ms < 0 || self.end_ms < 0 {
            return Err(CaptureFailure {
                kind: CaptureFailureKind::InvalidWindow,
                message: format!(
                    "Sample window cannot have negative timestamps: start_ms={}, end_ms={}",
                    self.start_ms, self.end_ms
                ),
                metrics: CaptureMetrics::default(),
            });
        }
        if self.start_ms >= self.end_ms {
            return Err(CaptureFailure {
                kind: CaptureFailureKind::InvalidWindow,
                message: format!(
                    "Sample window must have start_ms < end_ms: start_ms={}, end_ms={}",
                    self.start_ms, self.end_ms
                ),
                metrics: CaptureMetrics::default(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureRequest {
    pub path: PathBuf,
    pub window: SampleWindow,
    pub audio_stream_index: Option<u32>,
    pub process_deadline_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputBytesSource {
    AvioInput,
    FixtureOrigin,
}

/// Header facts from the source-versioned metadata probe, distinct from decoded PTS.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureSourceFacts {
    pub audio_stream_index: u32,
    pub format_start_time_us: Option<i64>,
    pub stream_start_pts: Option<i64>,
    pub stream_time_base: Option<String>,
    #[serde(default)]
    pub codec_name: Option<String>,
    #[serde(default)]
    pub native_sample_rate: Option<u32>,
    #[serde(default)]
    pub audio_channels: Option<u32>,
    pub provenance: String,
}

/// A same-demux packet/frame proof; bounds retain source-clock quantization.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuantizedPcmMapping {
    pub origin_min_us: i64,
    pub origin_max_us: i64,
    pub native_sample_rate: u32,
    pub source_tick_us: u32,
    pub packet_count: u64,
    pub block_anchors: u64,
    pub decoded_native_samples: u64,
    pub codec_native_samples: u64,
    pub encoded_payload_sha256: String,
    pub proof_version: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PcmTimeline {
    #[default]
    Unknown,
    Contiguous,
    Invalid(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureMetrics {
    #[serde(default)]
    pub quantized_mapping: Option<QuantizedPcmMapping>,
    pub pcm_frame_ms: i64,
    pub pcm_rms_dbfs_milli: Vec<i32>,
    pub audio_stream_index: Option<u32>,
    /// Unknown unless measured from source packets; never inferred from the seek request.
    pub source_start_pts_ms: Option<i64>,
    /// Proven source-timeline origin of contiguous decoded PCM. This is not the
    /// stream's initial PTS or the requested seek. Only a producer that measures
    /// decoded timing/continuity may set it. The final same-process observer
    /// establishes it only with a known container origin and a valid sample clock.
    /// None means unknown; pcm_timeline retains explicit discontinuities.
    pub contiguous_pcm_start_ms: Option<i64>,
    #[serde(default)]
    pub pcm_timeline: PcmTimeline,
    #[serde(default)]
    pub first_decoded_source_pts: Option<i64>,
    #[serde(default)]
    pub decoded_time_base: Option<String>,
    #[serde(default)]
    pub source_facts: Option<CaptureSourceFacts>,
    /// Durable job that actually captured this sample. Reuse never changes it.
    #[serde(default)]
    pub captured_job_id: Option<String>,
    #[serde(default)]
    pub remote_gate_wait_ms: Option<u64>,
    #[serde(default)]
    pub capture_process_ms: Option<u64>,
    #[serde(default)]
    pub metadata_elapsed_ms: Option<u64>,
    pub time_mapping_version: u32,
    pub elapsed_ms: u64,
    pub time_to_first_pcm_ms: Option<u64>,
    pub pcm_read_wait_us: u64,
    pub chromaprint_consume_us: u64,
    pub pcm_bytes: u64,
    #[serde(default)]
    pub pcm_sha256: Option<String>,
    pub input_bytes: Option<u64>,
    pub input_bytes_source: Option<InputBytesSource>,
    pub measurement_complete: bool,
    pub ffmpeg_exit_code: Option<i32>,

    // Benchmark / CPU diagnostics extensions
    pub command_setup_ms: Option<u64>,
    pub ffmpeg_spawn_ms: Option<u64>,
    pub pcm_stream_elapsed_ms: Option<u64>,
    pub chromaprint_finish_ms: Option<u64>,
    pub ffmpeg_wait_ms: Option<u64>,
    pub stderr_collect_ms: Option<u64>,
    pub sample_count: Option<u64>,
    pub ffmpeg_user_cpu_ms: Option<u64>,
    pub ffmpeg_system_cpu_ms: Option<u64>,
    pub ffmpeg_real_ms: Option<u64>,
    pub ffmpeg_maxrss_kb: Option<u64>,
    pub ffmpeg_stderr_bytes: Option<u64>,
    pub ffmpeg_stderr_tail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedFingerprint {
    pub window: SampleWindow,
    pub words: Vec<u32>,
    pub pcm_duration_ms: Option<i64>,
    pub metrics: CaptureMetrics,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureFailureKind {
    Io,
    Timeout,
    EmptyAudio,
    InvalidWindow,
    Decode,
    Cancelled,
}

#[derive(Clone, Debug, Error, PartialEq, Eq, Serialize, Deserialize)]
#[error("{kind:?}: {message}")]
pub struct CaptureFailure {
    pub kind: CaptureFailureKind,
    pub message: String,
    pub metrics: CaptureMetrics,
}

pub trait FingerprintCaptureEngine: Send + Sync {
    /// Production has already inspected metadata. Native decoders may require
    /// resolved facts here; injected engines can provide their own evidence.
    fn capture_window_after_metadata_controlled(
        &self,
        request: &CaptureRequest,
        source: Option<&CaptureSourceFacts>,
        control: &crate::WorkControl,
    ) -> Result<CapturedFingerprint, CaptureFailure> {
        match source {
            Some(source) => self.capture_window_from_source_controlled(request, source, control),
            None => self.capture_window_controlled(request, control),
        }
    }
    fn capture_window_from_source_controlled(
        &self,
        request: &CaptureRequest,
        _source: &CaptureSourceFacts,
        control: &crate::WorkControl,
    ) -> Result<CapturedFingerprint, CaptureFailure> {
        self.capture_window_controlled(request, control)
    }
    fn capture_window_controlled(
        &self,
        request: &CaptureRequest,
        control: &crate::WorkControl,
    ) -> Result<CapturedFingerprint, CaptureFailure> {
        if control.is_cancelled() {
            return Err(CaptureFailure {
                kind: CaptureFailureKind::Cancelled,
                message: "Library work cancelled".into(),
                metrics: CaptureMetrics::default(),
            });
        }
        let result = self.capture_window(request);
        if control.is_cancelled() {
            return Err(CaptureFailure {
                kind: CaptureFailureKind::Cancelled,
                message: "Library work cancelled".into(),
                metrics: CaptureMetrics::default(),
            });
        }
        result
    }
    fn capture_window(
        &self,
        request: &CaptureRequest,
    ) -> Result<CapturedFingerprint, CaptureFailure>;
}
