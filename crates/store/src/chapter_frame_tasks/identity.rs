use domain::LedgerId;
use sha2::{Digest, Sha256};

use crate::StoreError;

/// Stable identity shared by persistence and HTTP. Ordering and duplicates do
/// not change the identity; validation belongs to the obligation writer.
pub fn chapter_frame_generation(
    file_id: LedgerId,
    source_version: &str,
    targets: &[(i64, i64)],
    renderer_version: u32,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"crawler-media/chapter-frame-generation/v1\0");
    for bytes in [file_id.to_string().as_bytes(), source_version.as_bytes()] {
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    let mut targets = targets.to_vec();
    targets.sort_unstable();
    targets.dedup();
    digest.update((targets.len() as u64).to_le_bytes());
    for (start, end) in targets {
        digest.update(start.to_le_bytes());
        digest.update(end.to_le_bytes());
    }
    digest.update(renderer_version.to_le_bytes());
    format!("{:x}", digest.finalize())
}

pub(super) fn normalize_targets(targets: &[(i64, i64)]) -> Result<Vec<(i64, i64)>, StoreError> {
    if targets
        .iter()
        .any(|&(start, end)| start < 0 || end <= start)
    {
        return Err(StoreError::Invalid("invalid_chapter_frame_target".into()));
    }
    let mut normalized = targets.to_vec();
    normalized.sort_unstable();
    normalized.dedup();
    Ok(normalized)
}
