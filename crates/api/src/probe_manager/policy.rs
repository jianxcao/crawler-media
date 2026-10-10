use std::path::Path;
use domain::MediaKind;
use store::Store;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeRequestOrigin {
    Filesystem,
    Detail,
    Playback,
    BackgroundRetry,
    ManualRetry,
    ManualRefresh,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeRequestResult {
    Complete,
    Queued { job_id: String },
    AlreadyRunning,
    Waiting { next_retry_at_ms: i64 },
    Exhausted,
    Disabled,
    Cancelled,
}

/// Evaluates whether a library allows automatic intro/outro detection and fingerprinting.
pub fn is_fingerprint_enabled_for_path(store: &Store, path: &Path, kind: MediaKind) -> bool {
    if kind != MediaKind::Tv {
        return false;
    }
    store
        .library_for_path(path, kind)
        .ok()
        .flatten()
        .is_some_and(|lib| lib.detect_intros && lib.enable_fingerprint)
}

/// Evaluates whether marker detection (the master switch) is enabled.
pub fn is_marker_detection_enabled_for_path(store: &Store, path: &Path, kind: MediaKind) -> bool {
    if kind != MediaKind::Tv {
        return false;
    }
    store
        .library_for_path(path, kind)
        .ok()
        .flatten()
        .is_some_and(|lib| lib.detect_intros)
}
