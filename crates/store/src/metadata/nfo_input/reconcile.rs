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
        let Some(existing) = self.existing_file_for_path(path)? else {
            return Ok(None);
        };
        let locale = crate::library_query::library_display_locale(self, existing.library_id)?
            .unwrap_or_default();
        let config_revision = self.library_config_revision(&existing.library_id.to_string())?;
        let target = NfoInputTarget {
            library_id: existing.library_id,
            media_id: existing.media_id,
            root_id: existing.root_id,
            path_key: existing.path_key,
            file_id: existing.file_id,
            version: FileVersion {
                revision: existing.file_revision,
                source_version: existing.source_version,
            },
        };
        Ok(Some(NfoInputReconcilePlan {
            path: path.to_path_buf(),
            kind: existing.kind,
            units: existing.units,
            locale,
            config_revision,
            target,
            key_of: {
                let aliases = crate::path_key::alias_snapshot(&self.library);
                std::sync::Arc::new(move |raw: &str| crate::path_key::canonical_key(raw, &aliases))
            },
        }))
    }

    /// Locked phase 3: record what [`NfoInputReconcilePlan::observe`] saw.
    ///
    /// `record_nfo_observations` CASes `source_version`/config revision, so an
    /// observation made stale while the lock was released is dropped, not written.
    pub fn apply_nfo_input_reconcile(
        &self,
        observed: NfoInputReconcileObserved,
    ) -> Result<bool, StoreError> {
        let NfoInputReconcileObserved { plan, observations } = observed;
        let path = plan.path.as_path();
        let target = &plan.target;
        let tx = crate::write_tx(&self.library)?;
        crate::library_operations::integrity_guard::ensure_integrity_writable(
            &tx,
            &[target.library_id],
        )?;
        crate::library_operations::integrity_guard::ensure_media_integrity_writable(
            &tx,
            target.media_id,
        )?;
        let before = tx.total_changes();
        if !record_nfo_observations(&tx, target, &observations)? {
            tracing::warn!(
                path = %path.display(),
                "NFO sidecar 观察在落库前已过期：本次不更新输入，留待下一次扫描"
            );
            return Ok(false);
        }
        // 内容没变时 `record_nfo_observations` 不写库：别为每次重扫白白重算一遍卡片。
        if tx.total_changes() == before {
            return Ok(false);
        }
        crate::library_write::reduce_library_media(&tx, target.library_id, target.media_id)?;
        tx.commit()?;
        tracing::info!(
            path = %path.display(),
            candidates = observations.len(),
            "NFO sidecar 变化已重算卡片"
        );
        Ok(true)
    }

    fn existing_file_for_path(&self, path: &Path) -> Result<Option<ExistingFile>, StoreError> {
        let key = crate::path_key::canonical_recorded_key(&self.library, &path.to_string_lossy());
        let row = self
            .library
            .query_row(
                "SELECT file_id, library_id, media_id, root_id, kind, file_revision, source_version, path_key
                 FROM library_files WHERE path_key = norm_path_key(?1) LIMIT 1",
                params![key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, String>(7)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            file_id,
            library_id,
            media_id,
            root_id,
            kind,
            file_revision,
            source_version,
            path_key,
        )) = row
        else {
            return Ok(None);
        };
        let file_id: LedgerId = file_id.parse()?;
        Ok(Some(ExistingFile {
            library_id: library_id.parse()?,
            media_id: media_id.parse()?,
            root_id,
            kind: kind.parse().map_err(StoreError::MediaKind)?,
            file_revision,
            source_version: source_version.unwrap_or_default(),
            units: self.units_of_file(file_id)?,
            file_id,
            path_key,
        }))
    }

    /// Preserve the file's recorded coverage, including an unnumbered TV file's empty set.
    fn units_of_file(&self, file_id: LedgerId) -> Result<Vec<UnitKey>, StoreError> {
        let mut statement = self.library.prepare(
            "SELECT season, episode FROM library_file_units
             WHERE file_id = ?1 ORDER BY season, episode",
        )?;
        statement
            .query_map(params![file_id.to_string()], |row| {
                Ok(UnitKey::new(row.get::<_, i32>(0)?, row.get::<_, i32>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
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
