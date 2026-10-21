//! Observing NFO sidecars: cold IO + parse, executed outside the Store mutex.

use std::path::Path;

use domain::{EntityScope, FileVersion, MediaKind, MetadataFields, UnitKey};
use library::nfo::{
    NfoReadOutcome, nfo_sidecars, parse_nfo_fields, parse_nfo_header, resolve_nfo_scope,
};
use sha2::{Digest, Sha256};

use super::{NfoObservation, NfoParseState};

/// Observe every NFO sidecar of one video file, one [`NfoObservation`] per candidate.
///
/// 候选不存在时也会产出一条 `Missing` 观察：发布阶段据此按 `input_key` 精确删除那份
/// sidecar 留下的旧输入（"确认 sidecar 被删掉 ⇒ 解除它对应的输入"），而不会顺手清掉
/// 同一媒体其它实体层的输入。坐标对不上已知覆盖时 `state = Unknown`，绝不回落到 Work。
pub fn observe_nfo_inputs(
    path: &Path,
    kind: MediaKind,
    units: &[UnitKey],
    locale: &str,
    version: &FileVersion,
    config_revision: i64,
) -> Vec<NfoObservation> {
    let mut observations = Vec::new();
    for sidecar in nfo_sidecars(path, kind) {
        let input_key = sidecar.path.to_string_lossy().into_owned();
        let scope = observe_one(path, units, &sidecar.path, sidecar.layer);
        match scope {
            Err(state) => observations.push(NfoObservation::new(
                input_key,
                sidecar.path,
                locale,
                version,
                config_revision,
                EntityScope::Work,
                state,
                None,
                MetadataFields::default(),
            )),
            Ok((scope, state, digest, fields)) => observations.push(NfoObservation::new(
                input_key,
                sidecar.path,
                locale,
                version,
                config_revision,
                scope,
                state,
                digest,
                fields,
            )),
        }
    }
    observations
}

/// Read and resolve one sidecar. `Err(state)` carries Missing/Unknown without an entity.
#[allow(clippy::type_complexity)]
fn observe_one(
    video_path: &Path,
    units: &[UnitKey],
    sidecar: &Path,
    layer: library::nfo::NfoLayer,
) -> Result<(EntityScope, NfoParseState, Option<String>, MetadataFields), NfoParseState> {
    let bytes = match std::fs::read(sidecar) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(sidecar = %sidecar.display(), "NFO sidecar absent");
            return Err(NfoParseState::Missing);
        }
        Err(error) => {
            tracing::warn!(sidecar = %sidecar.display(), %error, "NFO sidecar unreadable");
            return Err(NfoParseState::Unknown);
        }
    };
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let Ok(body) = std::str::from_utf8(&bytes) else {
        tracing::warn!(sidecar = %sidecar.display(), "NFO sidecar is not UTF-8");
        return Err(NfoParseState::Unknown);
    };
    let scope = parse_nfo_header(body)
        .and_then(|header| resolve_nfo_scope(layer, video_path, units, &header));
    let Some(scope) = scope else {
        tracing::debug!(sidecar = %sidecar.display(), "NFO sidecar coordinates are unclear");
        return Err(NfoParseState::Unknown);
    };
    let (state, fields) = match parse_nfo_fields(body, scope) {
        NfoReadOutcome::Valid(input) => (NfoParseState::Valid, input.fields),
        NfoReadOutcome::Invalid { message } => {
            tracing::warn!(sidecar = %sidecar.display(), %message, "NFO sidecar is invalid");
            (NfoParseState::Invalid, MetadataFields::default())
        }
        NfoReadOutcome::Unknown { .. } => (NfoParseState::Unknown, MetadataFields::default()),
        NfoReadOutcome::Missing => (NfoParseState::Missing, MetadataFields::default()),
    };
    Ok((scope, state, Some(digest), fields))
}
