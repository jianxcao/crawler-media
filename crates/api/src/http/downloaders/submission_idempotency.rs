use axum::http::StatusCode;
use axum::response::Response;
use domain::{DownloaderId, Release, Torrent};

use super::submission::{SubmissionDelivery, SubmitFailure, subscribe_config_changed};
use crate::http::err;
use crate::management::ApiState;

pub(super) struct ManualSubmitOutcome {
    pub(super) operation_id: Option<domain::PluginOperationId>,
    pub(super) dl_id: Option<DownloaderId>,
    pub(super) info_hash: Option<String>,
    pub(super) is_unknown: bool,
    pub(super) already_owned: bool,
}

fn record_submitted_pending(
    state: &ApiState,
    subscribe: &domain::Subscribe,
    torrent: &Torrent,
    downloader_id: Option<DownloaderId>,
    parsed: &Release,
    score: i32,
    submitted_at: Option<i64>,
    info_hash: Option<&str>,
) -> Result<(), Response> {
    let store = state.store.as_ref();
    state.require_writable()?;
    state.require_write_admitted()?;
    if let Err(error) = store.record_pending_submission(
        subscribe.id,
        score,
        &crate::store::PendingDownload {
            torrent: torrent.clone(),
            release_override: None,
            downloader_id,
            submitted_at: Some(submitted_at.unwrap_or_else(crate::job_loop::unix_now)),
        },
    ) {
        tracing::error!(%error, subscribe_id = %subscribe.id, torrent = %torrent.title, "保存已提交的种子失败");
        return Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ));
    }
    if let Some(hash) = info_hash.map(str::trim).filter(|s| !s.is_empty())
        && let Err(error) = store.set_pending_info_hash(subscribe.id, &torrent.enclosure, hash)
    {
        tracing::warn!(%error, subscribe_id = %subscribe.id, "更新手动提交 info_hash 失败");
    }
    let units = parsed.covered_episodes();
    if units.is_empty() {
        let _ = store.record_wanted_grab(
            subscribe.id,
            parsed.season,
            parsed.episode,
            &torrent.title,
            crate::job_loop::unix_now(),
        );
    } else {
        for (season, episode) in units {
            let _ = store.record_wanted_grab(
                subscribe.id,
                Some(season),
                Some(episode),
                &torrent.title,
                crate::job_loop::unix_now(),
            );
        }
    }
    Ok(())
}

impl SubmissionDelivery {
    fn rollback_untracked_delivery(
        &self,
        operation_id: Option<domain::PluginOperationId>,
        dl_id: Option<DownloaderId>,
        info_hash: Option<&str>,
    ) {
        if let Ok(client) = crate::delivery::client_for_id(&self.state, dl_id) {
            let del_res = match info_hash.map(str::trim).filter(|s| !s.is_empty()) {
                Some(hash) => match client.delete_task(hash, false) {
                    Err(downloader::DownloaderError::Message(ref m))
                        if m == "delete_task not supported" =>
                    {
                        client.remove_owned(&self.torrent, false)
                    }
                    other => other,
                },
                None => client.remove_owned(&self.torrent, false),
            };
            if let Err(error) = del_res {
                tracing::error!(%error, torrent=%self.torrent.title, "Failed to roll back untracked manual delivery");
            }
        }
        let Some(op_id) = operation_id else {
            return;
        };
        let Ok(Some(op_row)) = self.state.store.get_download_operation(&op_id.to_string()) else {
            return;
        };
        let Some(rec_dl_id) = op_row.downloader_id.or(dl_id) else {
            return;
        };
        let _ = self
            .state
            .store
            .record_download_ownership(&store::RecordDownloadOwnershipInput {
                downloader_id: rec_dl_id,
                ownership_tag: op_row.ownership_tag,
                info_hash: op_row.info_hash.or_else(|| info_hash.map(str::to_string)),
                operation_id: op_row.operation_id,
                subscribe_id: op_row.subscribe_id,
                plugin_id: op_row.plugin_id,
                subject: op_row.subject,
                site_id: op_row.site_id,
                enclosure: op_row.enclosure,
                ownership_state: store::DownloadOwnershipState::Deleted,
                provenance_kind: store::DownloadProvenanceKind::CreatedFresh,
                verified_bytes: op_row.verified_bytes,
                bytes_digest: op_row.bytes_digest,
                now_utc: crate::job_loop::unix_now(),
            });
    }

    pub(super) fn replay_http_manual_if_present(
        &self,
    ) -> Result<Option<Option<DownloaderId>>, SubmitFailure> {
        if self.http_idem_key.is_none() {
            return Ok(None);
        }
        let score = self.subscribe_score();
        let (plan, op_ctx) = self.build_manual_plan_and_ctx(score)?;
        let initial_dl_id = self
            .downloader_id
            .or_else(|| {
                crate::delivery::target_id_from_store(
                    &self.state.store,
                    self.subscribe.as_ref(),
                    &self.torrent,
                )
                .ok()
                .flatten()
            })
            .unwrap_or_else(|| DownloaderId::from_uuid(uuid::Uuid::nil()));
        let expected_digest = crate::download_service::compute_request_digest(&plan, initial_dl_id);
        let mut existing = None;
        for attempt in 0usize.. {
            let candidate_key = if attempt == 0 {
                op_ctx.idempotency_key.clone()
            } else {
                format!("{}#rb:{attempt}", op_ctx.idempotency_key)
            };
            let row = match self
                .state
                .store
                .get_download_operation_by_idempotency(&op_ctx.subject, &candidate_key)
            {
                Ok(Some(row)) => row,
                Ok(None) => break,
                Err(
                    store::StoreError::WriteBusy
                    | store::StoreError::ReadBusy
                    | store::StoreError::ReadDeadline,
                ) => {
                    return Err(SubmitFailure::Response(
                        crate::http::store_wait::busy_response(),
                    ));
                }
                Err(error) => {
                    return Err(SubmitFailure::Response(err(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "store.error",
                        &error.to_string(),
                    )));
                }
            };
            if row.request_digest != expected_digest {
                return Err(SubmitFailure::Downloader(
                    downloader::DownloaderError::Message(
                        "persistence error: idempotency key reused for a different plugin request"
                            .to_string(),
                    ),
                ));
            }
            if crate::download_service::is_operation_rolled_back(&self.state, &row) {
                continue;
            }
            existing = Some(row);
            break;
        }
        let Some(existing) = existing else {
            return Ok(None);
        };
        let Some(outcome) = crate::download_service::replay_outcome(&existing) else {
            return Ok(None);
        };
        self.apply_replayed_outcome(outcome).map(Some)
    }

    fn apply_replayed_outcome(
        &self,
        outcome: downloader::DownloadOutcome,
    ) -> Result<Option<DownloaderId>, SubmitFailure> {
        match outcome {
            downloader::DownloadOutcome::Submitted {
                operation_id,
                downloader_id,
                info_hash,
                already_owned,
                ..
            } => {
                let dl_id = downloader_id.or(self.downloader_id);
                self.ensure_replayed_pending(
                    ManualSubmitOutcome {
                        operation_id: Some(operation_id),
                        dl_id,
                        info_hash,
                        is_unknown: false,
                        already_owned,
                    },
                    false,
                )?;
                Ok(dl_id)
            }
            downloader::DownloadOutcome::Unknown {
                operation_id,
                downloader_id,
                ..
            } => {
                let dl_id = downloader_id.or(self.downloader_id);
                self.ensure_replayed_pending(
                    ManualSubmitOutcome {
                        operation_id: Some(operation_id),
                        dl_id,
                        info_hash: None,
                        is_unknown: true,
                        already_owned: false,
                    },
                    true,
                )?;
                Ok(dl_id)
            }
            downloader::DownloadOutcome::Rejected { reason, .. } => {
                let msg = self.sanitize_submit_reason(&reason);
                Err(SubmitFailure::Downloader(
                    downloader::DownloaderError::Message(msg),
                ))
            }
            downloader::DownloadOutcome::Deferred { reason, .. } => {
                let msg = self.sanitize_submit_reason(&reason);
                Err(SubmitFailure::Response(err(
                    StatusCode::CONFLICT,
                    "plugin.deferred",
                    &msg,
                )))
            }
        }
    }

    fn ensure_replayed_pending(
        &self,
        info: ManualSubmitOutcome,
        allow_zero_ts: bool,
    ) -> Result<(), SubmitFailure> {
        let Some(sub) = &self.subscribe else {
            return Ok(());
        };
        if self.state.subscribe_is_deleting(sub.id) {
            return Err(super::submission::deleting_subscribe_failure());
        }
        crate::admit::ensure_subscribe_library_available(&self.state, sub)
            .map_err(SubmitFailure::Response)?;
        let recorded = self.state.store.load_pending(sub.id).is_ok_and(|rows| {
            rows.iter().any(|(_, p)| {
                p.torrent.enclosure == self.torrent.enclosure
                    && (allow_zero_ts || p.submitted_at.is_some())
            })
        });
        if recorded {
            return Ok(());
        }
        let score = self.subscribe_score();
        self.persist_or_rollback_pending(info, score)
    }

    fn guard_subscribe_post_submit(
        &self,
        sub: &domain::Subscribe,
    ) -> Result<domain::Subscribe, Response> {
        if self.state.subscribe_is_deleting(sub.id) {
            return Err(err(
                StatusCode::CONFLICT,
                "subscription.deleting",
                "订阅正在删除",
            ));
        }
        let current = match self.state.store.get_subscribe(sub.id) {
            Ok(Some(current)) => current,
            Ok(None) => {
                return Err(err(
                    StatusCode::CONFLICT,
                    "subscription.deleting",
                    "订阅正在删除",
                ));
            }
            Err(
                store::StoreError::WriteBusy
                | store::StoreError::ReadBusy
                | store::StoreError::ReadDeadline,
            ) => {
                return Err(crate::http::store_wait::busy_response());
            }
            Err(error) => {
                return Err(err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                ));
            }
        };
        if subscribe_config_changed(&current, sub) {
            return Err(err(
                StatusCode::CONFLICT,
                "subscription.changed",
                "订阅配置已变更，请刷新后重试",
            ));
        }
        crate::admit::ensure_subscribe_library_available(&self.state, &current)?;
        Ok(current)
    }

    pub(super) fn classify_cancelled_submit(&self) -> SubmitFailure {
        let Some(sub) = self.subscribe.as_ref() else {
            return SubmitFailure::Response(err(
                StatusCode::CONFLICT,
                "library.unavailable",
                "目标媒体库不可写入",
            ));
        };
        let guard = self.state.subscribe_guard(sub.id);
        let _held = guard.lock();
        match self.guard_subscribe_post_submit(sub) {
            Err(response) => SubmitFailure::Response(response),
            Ok(current) if current.tracking_state == "paused" && sub.tracking_state == "paused" => {
                SubmitFailure::Response(err(
                    StatusCode::CONFLICT,
                    "subscription.paused",
                    "订阅已暂停，无法运行",
                ))
            }
            Ok(current) if &current != sub => SubmitFailure::Response(err(
                StatusCode::CONFLICT,
                "subscription.changed",
                "订阅配置已变更，请刷新后重试",
            )),
            Ok(_) => SubmitFailure::Response(err(
                StatusCode::CONFLICT,
                "library.unavailable",
                "目标媒体库不可写入",
            )),
        }
    }

    pub(super) fn persist_or_rollback_pending(
        &self,
        info: ManualSubmitOutcome,
        score: i32,
    ) -> Result<(), SubmitFailure> {
        let Some(sub) = &self.subscribe else {
            return Ok(());
        };
        let guard = self.state.subscribe_guard(sub.id);
        let held = guard.lock();
        if let Err(response) = self.guard_subscribe_post_submit(sub) {
            drop(held);
            if !info.already_owned && !info.is_unknown {
                self.rollback_untracked_delivery(
                    info.operation_id,
                    info.dl_id,
                    info.info_hash.as_deref(),
                );
            }
            return Err(SubmitFailure::Response(response));
        }
        if info.is_unknown
            && self.state.store.load_pending(sub.id).is_ok_and(|rows| {
                rows.iter()
                    .any(|(_, p)| p.torrent.enclosure == self.torrent.enclosure)
            })
        {
            return Ok(());
        }
        let submitted_at = if info.is_unknown {
            Some(0)
        } else {
            Some(crate::job_loop::unix_now())
        };
        if let Err(response) = record_submitted_pending(
            &self.state,
            sub,
            &self.torrent,
            info.dl_id,
            &self.parsed,
            score,
            submitted_at,
            info.info_hash.as_deref(),
        ) {
            drop(held);
            if !info.already_owned && !info.is_unknown {
                self.rollback_untracked_delivery(
                    info.operation_id,
                    info.dl_id,
                    info.info_hash.as_deref(),
                );
            }
            return Err(SubmitFailure::Response(response));
        }
        Ok(())
    }
}
