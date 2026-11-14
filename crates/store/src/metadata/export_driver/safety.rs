use super::{NfoExportRequest, NfoSidecarObservation, StoreError};

/// Refuse to replace a changed or unreadable sidecar, preserving its bytes.
pub(super) fn ensure_sidecar_replaceable(
    plan: &NfoExportRequest,
    observation: &NfoSidecarObservation,
) -> Result<(), StoreError> {
    let disk = observation.digest.as_deref();
    if disk != plan.expected_digest.as_deref() {
        return Err(StoreError::Conflict(format!(
            "nfo export {} refused: {} changed since the export snapshot",
            plan.export_id,
            plan.path.display()
        )));
    }
    if disk.is_none() {
        return Ok(());
    }
    if observation.parsed.is_none() {
        tracing::warn!(path = %plan.path.display(), "sidecar 不是可读 NFO，保留原字节不覆盖");
        return Err(StoreError::Conflict(format!(
            "nfo export {} refused to overwrite unreadable {}",
            plan.export_id,
            plan.path.display()
        )));
    }
    Ok(())
}
