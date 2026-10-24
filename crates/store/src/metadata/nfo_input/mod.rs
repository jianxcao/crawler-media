//! NFO inputs are observed outside the write transaction and published with optimistic CAS.
//!
//! 扫描路径先在事务外观察（`observe_nfo_inputs`，冷 IO/解析不进 `BEGIN IMMEDIATE`），再把结果交给
//! `record_nfo_observations` 在同一事务里按 CAS 校验后落库；已有文件的复核
//! （[`crate::Store::reconcile_nfo_inputs`]）由调用方在 Store 之外触发，sidecar 的 IO/解析仍在
//! Store 互斥锁内完成，这是当前已知的取舍。
//!
//! 一个视频文件旁边可能同时存在多份 NFO，各自描述不同的实体层：整部作品
//! （`tvshow.nfo` / `movie.nfo`）、某一季（`season.nfo`）、某一集（与视频同名的 `.nfo`）。
//! 因此观察结果是**每个候选一条**记录，`input_key` 取 sidecar 的规范路径（不是视频路径），
//! 发布时按「实体 + locale + input_key」精确 upsert/delete：一份 sidecar 只维护自己那一层。

mod observe;
mod reconcile;
mod record;

use std::path::PathBuf;

use domain::{EntityScope, FileVersion, MetadataFields};

pub use observe::observe_nfo_inputs;
pub(crate) use observe::{observe_nfo_inputs_with_aliases, observe_nfo_inputs_with_key};
pub use reconcile::{NfoInputReconcileObserved, NfoInputReconcilePlan};
pub use record::{NfoInputTarget, record_nfo_observations};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NfoParseState {
    Valid,
    Invalid,
    Unknown,
    Missing,
}

/// Owned NFO observation produced before acquiring the Store mutex.
///
/// `input_key` is the canonical sidecar path (the key every row is upserted/deleted by);
/// `sidecar_path` keeps the same path for observability. A candidate that does not exist on
/// disk is still observed, with `state = Missing`, so its old input can be released.
#[derive(Clone, Debug, PartialEq)]
pub struct NfoObservation {
    pub input_key: String,
    pub sidecar_path: Option<PathBuf>,
    pub locale: String,
    pub expected_source_version: String,
    pub expected_config_revision: i64,
    pub scope: EntityScope,
    pub state: NfoParseState,
    pub digest: Option<String>,
    pub fields: MetadataFields,
    pub provenance_json: String,
}

impl NfoObservation {
    pub(super) fn new(
        input_key: String,
        sidecar_path: PathBuf,
        locale: &str,
        version: &FileVersion,
        config_revision: i64,
        scope: EntityScope,
        state: NfoParseState,
        digest: Option<String>,
        fields: MetadataFields,
    ) -> Self {
        Self {
            input_key,
            sidecar_path: Some(sidecar_path),
            locale: locale.to_string(),
            expected_source_version: version.source_version.clone(),
            expected_config_revision: config_revision,
            scope,
            state,
            digest,
            fields,
            provenance_json: r#"{"kind":"nfo","authority":"library_sidecar"}"#.into(),
        }
    }
}
