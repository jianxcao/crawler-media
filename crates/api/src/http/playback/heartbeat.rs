//! Device-scoped Playback heartbeats.
use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::Value;

use super::{effective_watch_state, row_for, watch_state_json};
use crate::store::{SessionRow, StoreError, UNIT_WHOLE, UnitState};
use crate::{
    http::{err, ok},
    job_loop::unix_now,
    management::ApiState,
};

struct Heartbeat<'a> {
    body: &'a Value,
    user_id: domain::UserId,
    media_id: domain::MediaId,
    kind: domain::MediaKind,
    ledger_id: String,
    season: i32,
    episode: i32,
    duration_ms: Option<i64>,
    now: i64,
    now_ms: i64,
    source: domain::PhysicalSourceId,
    /// 当前物理源的 `source_version`,与 `source` 同一次读取得到。
    /// 未绑定会话的事件用它做 unit-only 提交(canonical 会自行复核)。
    source_version: String,
}

/// 未绑定会话的事件所用的哨兵会话 id。它不可能出现在 `playback_session_ids` 里,
/// 因此 canonical 的 `read_binding` 查空,事件只能落进 unit-only 提交路径:
/// 恰好一个 IMMEDIATE 事务、只写播放单元事实、绝不写 `playback_sessions`。
const UNBOUND_SESSION_ID: &str = "__unbound_playback_event__";

pub(crate) async fn progress(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Json(body): Json<Value>,
) -> Response {
    let device_id = body["device_id"].as_str().unwrap_or_default();
    if device_id.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "playback.invalid",
            "缺少 device_id",
        );
    }
    if let Some(rate_val) = body.get("rate") {
        if let Some(r) = rate_val.as_f64() {
            if !r.is_finite() || r < 0.25 || r > 4.0 {
                return err(
                    StatusCode::BAD_REQUEST,
                    "playback.invalid_rate",
                    "播放倍率必须在 0.25 到 4.0 之间",
                );
            }
        }
    }
    let play_session_id = body["play_session_id"].as_str().map(str::to_string);
    let event_sequence = body["event_sequence"].as_i64();
    if let Some(ref sid) = play_session_id {
        if sid.len() > 128 {
            return err(StatusCode::BAD_REQUEST, "playback.invalid", "play_session_id 超长");
        }
    }
    if let Some(seq) = event_sequence {
        if seq < 0 {
            return err(StatusCode::BAD_REQUEST, "playback.invalid", "event_sequence 必须非负");
        }
    }
    let (file, media, season, episode, duration_ms, source, source_version) = {
        let store = state.store.lock();
        match store.is_device_revoked(user_id, device_id) {
            Ok(true) => return err(StatusCode::FORBIDDEN, "playback.revoked", "该设备已被注销"),
            Ok(false) => {}
            Err(error) => return persistence_error(&error, user_id, device_id),
        }
        let Some((file, media, season, episode)) = row_for(&store, &body, Some(user_id)) else {
            return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
        };
        let Some(source) = store.visible_file(user_id, file.file_id).ok().flatten() else {
            return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
        };
        let duration = match unit_duration(&store, user_id, &file, season, episode) {
            Ok(duration) => body["duration_ms"].as_i64().filter(|d| *d > 0).or(duration),
            Err(error) => return persistence_error(&error, user_id, device_id),
        };
        let source_version = source.version.source_version.clone();
        (file, media, season, episode, duration,
            domain::PhysicalSourceId { file_id: source.file_id, revision: source.version.revision },
            source_version)
    };
    if duration_ms.is_none() {
        let coverage = domain::FileCoverage {
            file: file.clone(),
            units: vec![domain::UnitKey::new(season, episode)],
        };
        let _ = crate::http::library::ensure_probe_enqueued(&state, &coverage);
    }
    let now_ms = state.playback_clock.now_ms();
    let heartbeat = Heartbeat {
        body: &body,
        user_id,
        media_id: media.id,
        kind: media.kind,
        ledger_id: file.file_id.to_string(),
        season,
        episode,
        duration_ms,
        now: now_ms / 1000,
        now_ms,
        source,
        source_version,
    };
    apply_heartbeat(&state.store.lock(), &heartbeat, device_id)
}

fn persistence_error(error: &StoreError, user_id: domain::UserId, device_id: &str) -> Response {
    tracing::error!(%error, %user_id, device_id, "Playback persistence failed");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        "保存播放状态失败",
    )
}

fn unit_duration(
    store: &crate::Store,
    user: domain::UserId,
    file: &domain::LibraryFile,
    season: i32,
    episode: i32,
) -> Result<Option<i64>, StoreError> {
    if let Some(duration) = effective_watch_state(store, user, file.media_id, season, episode)?
        .and_then(|unit| unit.duration_ms)
    {
        return Ok(Some(duration));
    }
    Ok(store
        .get_file_meta(&file.file_id.to_string())?
        .and_then(|tracks| tracks.video.and_then(|video| video.duration_secs))
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .map(|seconds| (seconds * 1000.0) as i64))
}

fn session_matches_heartbeat(session: &SessionRow, heartbeat: &Heartbeat<'_>) -> bool {
    session.media_id == heartbeat.media_id
        && session.season.unwrap_or(UNIT_WHOLE) == heartbeat.season
        && session.episode.unwrap_or(UNIT_WHOLE) == heartbeat.episode
}

fn persist_heartbeat(
    store: &crate::Store,
    heartbeat: &Heartbeat<'_>,
    device: &str,
    resolved_session_id: Option<&str>,
) -> Result<(UnitState, bool), StoreError> {
    let unit_key = domain::UnitKey::new(heartbeat.season, heartbeat.episode);
    let ledger_id = match heartbeat.ledger_id.parse::<domain::LedgerId>() {
        Ok(id) => id,
        Err(_) => {
            return Ok((
                store.unit_state(heartbeat.user_id, heartbeat.media_id, heartbeat.season, heartbeat.episode)?
                    .unwrap_or_default(),
                true,
            ))
        }
    };

    // Never persist an event against a stale target: the physical file must still exist,
    // still belong to this Media and still cover the reported unit. Coverage membership is
    // the ownership fact, so a merged file serving E01/E02 is judged per unit.
    let stale = || -> Result<bool, StoreError> {
        let Some(file) = store.visible_file(heartbeat.user_id, ledger_id)? else {
            return Ok(true);
        };
        if file.media_id != heartbeat.media_id {
            return Ok(true);
        }
        if !store.file_covers_unit(ledger_id, heartbeat.media_id, unit_key)? {
            return Ok(true);
        }
        match store.file_owner_library(ledger_id)? {
            Some(owner) => Ok(!store.library_accepts_work(&owner.id)?),
            None => Ok(false),
        }
    };
    if stale()? {
        tracing::warn!(ledger_id = %heartbeat.ledger_id, media_id = %heartbeat.media_id,
            "Ignoring stale Playback heartbeat: target or coverage changed");
        return Ok((
            store.unit_state(heartbeat.user_id, heartbeat.media_id, heartbeat.season, heartbeat.episode)?
                .unwrap_or_default(),
            true,
        ));
    }

    let event = heartbeat.body["event"].as_str().unwrap_or("progress");
    let previous = store.get_session(heartbeat.user_id, device)?;
    let play_session_id = heartbeat.body["play_session_id"].as_str().map(str::to_string);
    let event_sequence = heartbeat.body["event_sequence"].as_i64();

    // 生效会话 id:客户端显式携带的优先,否则用事务外已签发(或本次签发)的 id。
    // 必须用「生效 id」查绑定,否则 legacy 无 id 客户端签发出来的会话不会流经
    // canonical,源被替换后就没人能判 IgnoredStale。
    let session_id = play_session_id.clone().or_else(|| resolved_session_id.map(str::to_string));
    let binding = match &session_id {
        Some(sid) => store.play_session_binding(heartbeat.user_id, device, sid)?,
        None => None,
    };

    let identity = crate::playback_events::PlaybackEventIdentity {
        play_session_id: play_session_id.clone(),
        event_sequence,
    };

    let event_kind = match event {
        "start" => crate::playback_events::PlaybackEventKind::Start,
        "stop" => crate::playback_events::PlaybackEventKind::Stop,
        _ => crate::playback_events::PlaybackEventKind::Progress,
    };

    let unit = unit_key;
    // 只读判定:管理员是否已结束本设备的播放。真正的收尾(归档 live 行、置
    // `playback_session_ids.state = 'closed'`)由 canonical 在自己的事务里做
    // (见 store `commit_playback_event` 的 admin-ended 守卫);这里只用于回包。
    let session_admin_ended = previous.as_ref().is_some_and(|p| p.admin_ended);
    let was_admin_ended = if event != "start" {
        session_admin_ended
            || store
                .device_was_admin_ended(heartbeat.user_id, device, heartbeat.now, 10)
                .unwrap_or(false)
    } else {
        false
    };
    // 回包口径与旧实现一致:`session_admin_ended` 覆盖 start(管理员结束后的 start 由
    // canonical 收尾),`was_admin_ended` 覆盖结束后的非 start 心跳。
    let admin_ended = session_admin_ended || was_admin_ended;

    let decision = crate::playback_events::classify_event_with_target(
        previous.as_ref(),
        binding.as_ref(),
        &identity,
        ledger_id,
        Some(unit),
        event_kind,
    );

    if decision == crate::playback_events::EventDecision::Ignore {
        let current = store.unit_state(heartbeat.user_id, heartbeat.media_id, heartbeat.season, heartbeat.episode)?.unwrap_or_default();
        return Ok((current, admin_ended));
    }
    // canonical 是这次事件的唯一写路径:它自己开一个 IMMEDIATE 事务,并在事务内
    // 复核会话绑定、物理源版本、Library 可达性与播放单元坐标。API 层不再叠加任何
    // 写操作,也不再开第二个事务。
    let item = if heartbeat.season == UNIT_WHOLE {
        domain::TypedItemId::Work(heartbeat.media_id)
    } else {
        domain::TypedItemId::Unit {
            media: heartbeat.media_id,
            key: domain::UnitKey::new(heartbeat.season, heartbeat.episode),
        }
    };
    // The saved resume point / duration are only valid for the physical source they were
    // recorded against: resolve them for *this* source instead of reading the bare unit row.
    let resolved = store
        .resolve_playback(heartbeat.user_id, &item, Some(&heartbeat.source), None)?;
    let same_source = resolved.target.file.file_id == heartbeat.source.file_id
        && resolved.target.file.version.revision == heartbeat.source.revision;
    let position_ms = heartbeat.body["position_ms"]
        .as_i64()
        .or_else(|| (event == "start" && same_source).then_some(resolved.start_ms))
        .unwrap_or(0)
        .max(0);
    let duration_ms = heartbeat
        .duration_ms
        .or(same_source.then_some(resolved.duration_ms).flatten());
    let event_type = match event {
        "start" => store::playback::PlaybackEventType::Playing,
        "stop" => store::playback::PlaybackEventType::Stopped,
        _ => store::playback::PlaybackEventType::Progress,
    };
    // 有绑定时交签发时记录的源事实:源被替换后 canonical 判 IgnoredStale。
    // 没有绑定时交本次读到的当前源事实,canonical 走 unit-only 分支:只写播放
    // 单元事实,绝不写 `playback_sessions`。
    let (event_session_id, source_id, source_version, generation) = match &binding {
        Some(b) => (
            session_id.clone().unwrap_or_default(),
            domain::PhysicalSourceId {
                file_id: b.ledger_id,
                revision: b.file_revision.unwrap_or_default(),
            },
            b.source_version.clone().unwrap_or_default(),
            b.generation,
        ),
        None => (
            UNBOUND_SESSION_ID.to_string(),
            heartbeat.source.clone(),
            heartbeat.source_version.clone(),
            0,
        ),
    };
    let bound_event = store::playback::BoundPlaybackEvent {
        user_id: heartbeat.user_id,
        device_id: device.to_string(),
        session_id: event_session_id.clone(),
        generation,
        event_sequence,
        item,
        source_id,
        source_version,
        position_ms,
        duration_ms,
        audio_track: heartbeat.body["audio_track"].as_str().map(str::to_string),
        subtitle_track: heartbeat.body["subtitle_track"].as_str().map(str::to_string),
        paused: heartbeat.body["paused"].as_bool().unwrap_or(false),
        rate_bps: 0,
        client: heartbeat.body["client"].as_str().map(str::to_string),
        device_name: heartbeat.body["device_name"].as_str().map(str::to_string),
        client_version: heartbeat.body["client_version"].as_str().map(str::to_string),
        play_method: None,
        event_type,
    };

    match store.commit_playback_event(&bound_event, heartbeat.now_ms)? {
        store::playback::EventOutcome::Applied => {
            tracing::debug!(session_id = %event_session_id, "commit_playback_event applied");
        }
        store::playback::EventOutcome::IgnoredStale => {
            // 卡 必做#2:判 IgnoredStale 后不得再有任何会话、位置或单元写入。
            tracing::warn!(session_id = %event_session_id, "commit_playback_event ignored as stale");
        }
    }
    let current = store.unit_state(heartbeat.user_id, heartbeat.media_id, heartbeat.season, heartbeat.episode)?.unwrap_or_default();
    Ok((current, admin_ended))
}

fn apply_heartbeat(store: &crate::Store, heartbeat: &Heartbeat<'_>, device: &str) -> Response {
    // 会话签发必须发生在 `commit_playback_event` 的 IMMEDIATE 事务之外:签发自己
    // 开一个 immediate 事务,嵌进去就是 SQLite 的 cannot-start-a-transaction-in-
    // a-transaction。
    let resolved_session_id = if heartbeat.body["play_session_id"].as_str().is_some() {
        None
    } else {
        let reusable = store
            .get_session(heartbeat.user_id, device)
            .ok()
            .flatten()
            .filter(|session| session_matches_heartbeat(session, heartbeat))
            .and_then(|session| session.play_session_id);
        match reusable {
            Some(sid) => Some(sid),
            None => {
                // plan:135 stop/progress 不伪造签发新的 Pending session，仅 start 允许按需签发
                let event = heartbeat.body["event"].as_str().unwrap_or("progress");
                if event == "start" {
                    heartbeat
                        .ledger_id
                        .parse::<domain::LedgerId>()
                        .ok()
                        .and_then(|ledger_id| {
                            store.visible_file(heartbeat.user_id, ledger_id).ok().flatten().and_then(|file| {
                                store.issue_play_session(
                                    heartbeat.user_id, device, ledger_id, heartbeat.media_id,
                                    domain::UnitKey::new(heartbeat.season, heartbeat.episode),
                                    &file.version, heartbeat.now,
                                ).ok()
                            })
                        })
                } else {
                    None
                }
            }
        }
    };
    match persist_heartbeat(store, heartbeat, device, resolved_session_id.as_deref()) {
        Ok((unit, ended)) => ok(watch_state_json(&unit, ended)).into_response(),
        Err(error) => persistence_error(&error, heartbeat.user_id, device),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_heartbeat_after_canonical_rebind_does_not_write_old_media() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::Store::open(temp.path()).unwrap();
        let library = store.list_libraries().unwrap().into_iter()
            .find(|library| library.kind == domain::MediaKind::Movie).unwrap();
        let provisional = domain::Media { id: domain::MediaId::new(), kind: domain::MediaKind::Movie,
            title: "provisional".into(), year: None, original_title: None, aliases: Vec::new(), tmdb_id: None,
            douban_id: None, tvdb_id: None, bangumi_id: None, anilist_id: None };
        let canonical = domain::Media { id: domain::MediaId::new(), kind: domain::MediaKind::Movie,
            title: "canonical".into(), year: None, original_title: None, aliases: Vec::new(), tmdb_id: Some("123".into()),
            douban_id: None, tvdb_id: None, bangumi_id: None, anilist_id: None };
        store.insert_media(&provisional).unwrap();
        store.insert_media(&canonical).unwrap();
        let file_id = domain::LedgerId::new();
        let path = library.root_paths[0].join("movie.mkv").display().to_string();
        let spec = store::test_support::FixtureFile {
            file_id: Some(file_id),
            media_id: provisional.id,
            path: path.clone(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: domain::QualitySource::Probe,
            confidence: domain::Confidence::High,
            filter_score: None,
            transfer_mode: None,
            source_path: None,
        };
        store::test_support::publish_fixture_file(&store, &spec).unwrap();
        store.rebind_scanned_ledger(file_id, canonical.id).unwrap();
        let user_id = domain::UserId::new();
        let body = serde_json::json!({"event":"start", "position_ms":500, "device_id":"race"});
        let file = store
            .published_files(&[file_id])
            .unwrap()
            .remove(&file_id)
            .expect("rebound fixture file is published");
        let heartbeat = Heartbeat { body: &body, user_id, media_id: provisional.id,
            kind: domain::MediaKind::Movie, ledger_id: file_id.to_string(),
            season: UNIT_WHOLE, episode: UNIT_WHOLE, duration_ms: Some(1000), now: 10, now_ms: 10_000,
            source_version: file.version.source_version.clone(),
            source: domain::PhysicalSourceId { file_id: file.file_id, revision: file.version.revision } };

        let (_, ended) = persist_heartbeat(&store, &heartbeat, "race", None).unwrap();

        assert!(ended);
        assert!(store.unit_state(user_id, provisional.id, UNIT_WHOLE, UNIT_WHOLE).unwrap().is_none());
        assert!(store.get_session(user_id, "race").unwrap().is_none());
    }
}
