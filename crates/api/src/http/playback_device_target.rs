use axum::http::StatusCode;
use axum::response::Response;
use std::str::FromStr;

use crate::http::err;

#[derive(Default, serde::Deserialize)]
pub(crate) struct DeviceTargetQuery {
    pub(crate) user_id: Option<String>,
}

pub(crate) fn resolve_device_target(
    store: &crate::Store,
    acting_user: domain::UserId,
    device_id: &str,
    query: &DeviceTargetQuery,
) -> Result<domain::UserId, Response> {
    let admin = store
        .user_role(acting_user)
        .map(|role| role == "admin")
        .unwrap_or(false);
    if !admin {
        if let Some(raw) = &query.user_id {
            if domain::UserId::from_str(raw).ok() != Some(acting_user) {
                return Err(err(
                    StatusCode::FORBIDDEN,
                    "auth.forbidden",
                    "不能操作其他成员的播放设备",
                ));
            }
        }
        return Ok(acting_user);
    }
    if let Some(raw) = &query.user_id {
        return domain::UserId::from_str(raw).map_err(|_| {
            err(
                StatusCode::BAD_REQUEST,
                "playback.user_invalid",
                "用户 id 无效",
            )
        });
    }
    let mut users: Vec<domain::UserId> = store
        .sessions_for_device(device_id)
        .unwrap_or_default()
        .into_iter()
        .map(|session| session.user_id)
        .collect();
    users.sort_by_key(|id| id.to_string());
    users.dedup();
    match users.as_slice() {
        [user_id] => Ok(*user_id),
        [] => Err(err(
            StatusCode::BAD_REQUEST,
            "playback.user_required",
            "缺少要操作的用户 id",
        )),
        _ => Err(err(
            StatusCode::CONFLICT,
            "playback.device_ambiguous",
            "多个成员使用了同一设备 id，请指定用户",
        )),
    }
}
