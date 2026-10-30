//! 已有文件的 sidecar 复核。
//!
//! `publish_files` 只在文件**首次**入库时带上观察结果；已有文件走的是「复用台账行」的既有
//! 分支，如果不在这里重新观察，sidecar 的编辑 / 损坏 / 删除就永远反映不到卡片上。这里读回
//! 该文件在库里的归属键，观察后在一个写事务里按「实体 + locale + input_key」精确落库。

use std::path::Path;

use domain::{FileVersion, LedgerId, LibraryId, MediaId, MediaKind, UnitKey};
use rusqlite::{OptionalExtension, params};

use super::{NfoInputTarget, record_nfo_observations};
use crate::Store;
use crate::StoreError;

/// 复核只需要文件在库里的归属键，不需要整份 `LibraryFile`。
struct ExistingFile {
    library_id: LibraryId,
    media_id: MediaId,
    root_id: String,
    file_id: LedgerId,
    kind: MediaKind,
    path_key: String,
    file_revision: i64,
    source_version: String,
    units: Vec<UnitKey>,
}

impl Store {
    /// 重新观察 `path` 旁边的所有 NFO 候选，并把变化落到该文件所在的媒体卡上。
    ///
    /// 返回是否真的改动了输入；路径没有台账行时返回 `Ok(false)`（新增文件由 publish 路径处理）。
    /// 观察在写事务外完成，事务内用 `source_version` CAS 兜底：观察期间文件或 Library 配置变了
    /// 就整批放弃，绝不用过期观察覆盖新状态。
    pub fn reconcile_nfo_inputs(&self, path: &Path) -> Result<bool, StoreError> {
        // Standalone convenience: resolve an existing caller spelling before the
        // DB-only plan. Shared Store callers use prepare/observe/apply separately.
        let key = crate::path_key::canonical_existing_key(&path.to_string_lossy());
        let Some(plan) = self.plan_nfo_input_reconcile(Path::new(&key))? else {
            return Ok(false);
        };
        let observed = plan.observe();
        self.apply_nfo_input_reconcile(observed)
    }

    /// Locked phase 1 of [`Self::reconcile_nfo_inputs`]: read the ownership keys.
    ///
    /// No file is touched. Callers that share the Store behind a mutex take the lock
    /// only for this call, run [`NfoInputReconcilePlan::observe`] off-lock, then lock
    /// again for [`Self::apply_nfo_input_reconcile`] (S01).
    pub fn plan_nfo_input_reconcile(
        &self,
        path: &Path,
    ) -> Result<Option<NfoInputReconcilePlan>, StoreError> {
        self.with_library_read(|db| db.plan_nfo_input_reconcile(path))
    }

    /// Locked phase 3: record what [`NfoInputReconcilePlan::observe`] saw.
    ///
    /// `record_nfo_observations` CASes `source_version`/config revision, so an
    /// observation made stale while the lock was released is dropped, not written.
    pub fn apply_nfo_input_reconcile(
        &self,
        observed: NfoInputReconcileObserved,
    ) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.apply_nfo_input_reconcile(observed))
    }
}

/// Ownership keys of one ledgered video, read under the Store lock.
#[derive(Clone)]
pub struct NfoInputReconcilePlan {
    path: std::path::PathBuf,
    kind: MediaKind,
    units: Vec<UnitKey>,
    locale: String,
    config_revision: i64,
    target: NfoInputTarget,
    key_of: std::sync::Arc<dyn Fn(&str) -> String + Send + Sync>,
}

/// Sidecar observations for a [`NfoInputReconcilePlan`], made without the Store.
pub struct NfoInputReconcileObserved {
    plan: NfoInputReconcilePlan,
    observations: Vec<super::NfoObservation>,
}

impl NfoInputReconcilePlan {
    /// Cold IO + parse of every sidecar candidate. Never touches the Store.
    pub fn observe(self) -> NfoInputReconcileObserved {
        let observations = super::observe_nfo_inputs_with_key(
            &self.path,
            self.kind,
            &self.units,
            &self.locale,
            &self.target.version,
            self.config_revision,
            |raw| (self.key_of)(raw),
        );
        NfoInputReconcileObserved {
            plan: self,
            observations,
        }
    }
}

impl NfoInputReconcileObserved {
    /// Consume the observations for a caller that commits them with file presence.
    pub fn into_observations(self) -> Vec<super::NfoObservation> {
        self.observations
    }
}

#[path = "reconcile/connection.rs"]
mod connection;
