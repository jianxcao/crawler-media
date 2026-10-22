//! Shared playback event classification, generation isolation, and validation.

use domain::LedgerId;
use store::{PlaySessionBinding, PlaySessionIdState, SessionRow};
pub mod clock;
pub use clock::{PlaybackClock, SystemPlaybackClock};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaybackEventIdentity {
    pub play_session_id: Option<String>,
    pub event_sequence: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackEventKind {
    Start,
    Progress,
    Stop,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventDecision {
    Start,
    Update,
    Stop,
    Ignore,
}

/// Classify an incoming playback event based on session identity, bindings, and previous session.
///
/// Implements the state machine in Section 2.3 and 2.4 of Batch 2 design:
/// - With PlaySessionId:
///   * Verified against server-issued binding (user, device, ledger).
///   * Start / Playing can activate a pending session and close older ones.
///   * Progress / Stop only updates an active session matching the exact session ID.
///   * EventSequence if present must be strictly greater than last_event_sequence.
///   * Unknown, closed, or mismatched ID events are ignored (return Ignore).
/// - Without PlaySessionId (Legacy client):
///   * Start creates/replaces legacy session for matching or new target.
///   * Progress / Stop only allowed if current active session matches the exact LedgerId.
///     If target differs or no current session, returns Ignore.
pub fn classify_event(
    previous: Option<&SessionRow>,
    binding: Option<&PlaySessionBinding>,
    identity: &PlaybackEventIdentity,
    ledger: LedgerId,
    event: PlaybackEventKind,
) -> EventDecision {
    classify_event_with_target(previous, binding, identity, ledger, None, event)
}

pub fn classify_event_with_target(
    previous: Option<&SessionRow>,
    binding: Option<&PlaySessionBinding>,
    identity: &PlaybackEventIdentity,
    ledger: LedgerId,
    target_unit: Option<domain::UnitKey>,
    event: PlaybackEventKind,
) -> EventDecision {
    let Some(session_id) = identity.play_session_id.as_deref() else {
        return classify_legacy(previous, ledger, event);
    };
    // Unrecognized or forged session ID -> Ignore
    let Some(b) = binding else {
        return EventDecision::Ignore;
    };
    if !binding_matches_target(b, ledger, target_unit) {
        return EventDecision::Ignore;
    }
    if sequence_regressed(previous, session_id, identity.event_sequence) {
        return EventDecision::Ignore;
    }
    classify_with_session_id(previous, b, session_id, event)
}

/// Binding must match target ledger and target unit (if specified).
///
/// `b.generation` 不校验:`playback_session_ids` 的唯一 INSERT 用 `COALESCE(MAX(generation), 0) + 1`,
/// 所以在本仓全部写入路径上 generation 恒 ≥ 1,本分支不可达;DDL 只要求 `CHECK(generation>=0)`
/// (`crates/store/src/schema/v1/library.sql:216`),允许 0,故只声明「本仓写入路径上不可达」。
fn binding_matches_target(
    b: &PlaySessionBinding,
    ledger: LedgerId,
    target_unit: Option<domain::UnitKey>,
) -> bool {
    if b.ledger_id != ledger {
        return false;
    }
    match target_unit {
        Some(unit) => b.unit == unit,
        None => true,
    }
}

/// Sequence check if provided within the same session.
fn sequence_regressed(
    previous: Option<&SessionRow>,
    session_id: &str,
    event_sequence: Option<i64>,
) -> bool {
    let (Some(prev), Some(seq)) = (previous, event_sequence) else {
        return false;
    };
    if prev.play_session_id.as_deref() != Some(session_id) {
        return false;
    }
    prev.last_event_sequence
        .is_some_and(|prev_seq| seq <= prev_seq)
}

/// ID-based playback session state machine (Section 2.3 of Batch 2 design).
fn classify_with_session_id(
    previous: Option<&SessionRow>,
    b: &PlaySessionBinding,
    session_id: &str,
    event: PlaybackEventKind,
) -> EventDecision {
    // Must match current active session row's play_session_id
    let same_session = previous.and_then(|p| p.play_session_id.as_deref()) == Some(session_id);
    match event {
        PlaybackEventKind::Start => match b.state {
            PlaySessionIdState::Pending => EventDecision::Start,
            PlaySessionIdState::Active if same_session => EventDecision::Update,
            PlaySessionIdState::Active => EventDecision::Start,
            PlaySessionIdState::Closed => EventDecision::Ignore,
        },
        PlaybackEventKind::Progress => match b.state {
            // A client that resumes and reports progress without a `start` still owns
            // an issued (pending) session: activate it and apply the same progress
            // instead of dropping the first real position the user watched.
            PlaySessionIdState::Pending => EventDecision::Start,
            PlaySessionIdState::Active if same_session => EventDecision::Update,
            _ => EventDecision::Ignore,
        },
        PlaybackEventKind::Stop => match b.state {
            PlaySessionIdState::Active if same_session => EventDecision::Stop,
            _ => EventDecision::Ignore,
        },
    }
}

/// Plan Task 4 Step 3: No-ID legacy playback session contract.
/// 首个 no-id `progress`(设备上没有任何 live 会话 ⇒ `previous == None`)判 `Update`:
/// 这就是 §16.3 ①「unbound 分支仍要写单元事实」① 的入口。canonical 的 unbound 分支
/// (`store/src/playback/event.rs:68-74` + `stage_unbound_event:144-177`)只写单元事实、不签发会话、不写播放日志,
/// 因此它既不伪造会话,也不让「进度事件」变成一次播放。
/// 判 `Ignore` 会让 `persist_heartbeat`(`http/playback/heartbeat.rs:254`)整段短路 ⇒
/// 心跳零写入,直接顶掉 `storage_contention` 的写作用域正控制
/// (`tests/storage_contention.rs:156`「基线心跳必须真的写库」)。
/// 跨目标陈旧保护不变:`previous` 存在但目标不匹配时仍是 `Ignore`。
/// stop 保持 `Ignore`(没有活动会话的 stop 不得伪造会话/日志)。
fn classify_legacy(
    previous: Option<&SessionRow>,
    ledger: LedgerId,
    event: PlaybackEventKind,
) -> EventDecision {
    match event {
        PlaybackEventKind::Start => EventDecision::Start,
        PlaybackEventKind::Progress => match previous {
            Some(prev) if !legacy_target_matches(prev, ledger) => EventDecision::Ignore,
            _ => EventDecision::Update,
        },
        PlaybackEventKind::Stop => match previous {
            Some(prev) if legacy_target_matches(prev, ledger) => EventDecision::Stop,
            _ => EventDecision::Ignore,
        },
    }
}

fn legacy_target_matches(prev: &SessionRow, ledger: LedgerId) -> bool {
    prev.ledger_id.as_deref() == Some(&ledger.to_string()) || prev.ledger_id.is_none()
}
