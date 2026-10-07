use std::str::FromStr;

use axum::http::StatusCode;
use axum::response::Response;
use domain::{Coverage, FetchMode, Subscribe};

use crate::http::err;
use crate::store::Store;

use super::types::PatchSubscriptionInput;

pub(super) fn apply_patch_fields(
    store: &Store,
    subscribe: &mut Subscribe,
    body: PatchSubscriptionInput,
) -> Result<(), Response> {
    apply_mode_and_flags(subscribe, &body)?;
    update_filter(
        store,
        subscribe,
        body.filter_id.as_ref().map(|opt| opt.as_deref()),
    )?;
    update_downloader(store, subscribe, body.downloader_id.as_deref())?;
    update_tracking_fields(subscribe, &body)?;
    update_selected_season(subscribe, body.selected_seasons)?;
    update_library_id(store, subscribe, body.library_id)?;
    subscribe
        .coverage
        .validate(subscribe.full_season_pack)
        .map_err(|message| err(StatusCode::BAD_REQUEST, "subscription.invalid", message))
}

fn update_filter(
    store: &Store,
    subscribe: &mut Subscribe,
    filter_id: Option<Option<&str>>,
) -> Result<(), Response> {
    let Some(filter_id_opt) = filter_id else {
        return Ok(());
    };
    let Some(filter_id) = filter_id_opt else {
        tracing::error!(subscribe_id = %subscribe.id, field = "filter_id", error = "null reference", "订阅更新拒绝无效规则组引用");
        return Err(err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "filter_id 不能为 null",
        ));
    };
    if filter_id.trim().is_empty() {
        tracing::error!(subscribe_id = %subscribe.id, field = "filter_id", filter_id = %filter_id, error = "empty reference", "订阅更新拒绝无效规则组引用");
        return Err(err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "filter_id 不能为空",
        ));
    }
    let id = domain::FilterId::from_str(filter_id).map_err(|error| {
        tracing::error!(subscribe_id = %subscribe.id, field = "filter_id", filter_id = %filter_id, error = %error, "订阅更新拒绝无效规则组引用");
        err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "filter_id 格式无效",
        )
    })?;
    match store.get_filter(id) {
        Ok(Some(_)) => {
            subscribe.filter_id = id;
            Ok(())
        }
        Ok(None) => {
            tracing::error!(subscribe_id = %subscribe.id, field = "filter_id", filter_id = %id, error = "reference not found", "订阅更新拒绝不存在的规则组引用");
            Err(err(
                StatusCode::BAD_REQUEST,
                "subscription.invalid",
                "引用的规则组不存在",
            ))
        }
        Err(error) => {
            tracing::error!(subscribe_id = %subscribe.id, filter_id = %id, error = %error, "订阅更新读取规则组失败");
            Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            ))
        }
    }
}

fn apply_mode_and_flags(
    subscribe: &mut Subscribe,
    body: &PatchSubscriptionInput,
) -> Result<(), Response> {
    if let Some(mode) = body.fetch_mode.as_deref() {
        subscribe.fetch_mode = FetchMode::from_str(mode).map_err(|_| {
            err(
                StatusCode::BAD_REQUEST,
                "subscription.invalid",
                "fetch_mode 无效",
            )
        })?;
    }
    if let Some(wash_cut) = body.wash_cut {
        subscribe.wash_cut = wash_cut;
    }
    if let Some(pack) = body.full_season_pack {
        subscribe.full_season_pack = pack;
    }
    if let Some(keep) = body.keep_old_versions {
        subscribe.keep_old_versions = keep;
    }
    Ok(())
}

fn update_downloader(
    store: &Store,
    subscribe: &mut Subscribe,
    downloader_id: Option<&str>,
) -> Result<(), Response> {
    let Some(downloader_id) = downloader_id else {
        return Ok(());
    };
    if downloader_id.is_empty() {
        subscribe.downloader_id = None;
        return Ok(());
    }
    let id = domain::DownloaderId::from_str(downloader_id).map_err(|_| {
        err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "downloader_id 无效",
        )
    })?;
    match store.get_downloader(id) {
        Ok(Some(_)) => subscribe.downloader_id = Some(id),
        Ok(None) => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "subscription.invalid",
                "downloader_id 无效",
            ));
        }
        Err(error) => {
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            ));
        }
    }
    Ok(())
}

fn update_library_id(
    store: &Store,
    subscribe: &mut Subscribe,
    library_id: Option<Option<String>>,
) -> Result<(), Response> {
    let Some(maybe_id) = library_id else {
        return Ok(());
    };
    let Some(raw) = maybe_id.as_deref().filter(|s| !s.is_empty()) else {
        subscribe.library_id = None;
        return Ok(());
    };
    let id = domain::LibraryId::from_str(raw).map_err(|_| {
        err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "library_id 无效",
        )
    })?;
    match store.get_library(raw) {
        Ok(Some(lib)) => {
            let media = store
                .get_media(subscribe.media_id)
                .map_err(|e| {
                    err(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "store.error",
                        &e.to_string(),
                    )
                })?
                .ok_or_else(|| {
                    err(
                        StatusCode::NOT_FOUND,
                        "subscription.missing",
                        "订阅关联的影视不存在",
                    )
                })?;
            if lib.kind != media.kind {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    "subscription.invalid",
                    "订阅媒体类型与目标库类型不匹配",
                ));
            }
            subscribe.library_id = Some(id);
        }
        Ok(None) => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "subscription.invalid",
                "指定的媒体库不存在",
            ));
        }
        Err(error) => {
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            ));
        }
    }
    Ok(())
}

fn update_tracking_fields(
    subscribe: &mut Subscribe,
    body: &PatchSubscriptionInput,
) -> Result<(), Response> {
    if let Some(state) = body
        .tracking_state
        .as_deref()
        .filter(|state| *state == "active" || *state == "paused")
    {
        subscribe.tracking_state = state.to_string();
    }
    if let Some(follow) = body.follow_future {
        subscribe.follow_future = follow;
    }
    if let Some(secs) = body.search_interval_secs {
        subscribe.search_interval_secs = crate::http::subscribe_schedule::clamp_interval(secs)
            .map_err(|message| err(StatusCode::BAD_REQUEST, "subscription.invalid", message))?;
    }
    Ok(())
}

fn update_selected_season(
    subscribe: &mut Subscribe,
    seasons: Option<Vec<u32>>,
) -> Result<(), Response> {
    let Some(seasons) = seasons else {
        return Ok(());
    };
    if seasons.len() != 1 || seasons[0] == 0 {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "每个订阅只能对应一季；请为其他季分别创建订阅",
        ));
    }
    if !matches!(subscribe.coverage, Coverage::Tv { .. }) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "selected_seasons 只对剧集订阅有效",
        ));
    }
    subscribe.coverage = Coverage::Tv {
        season: seasons[0],
        episode_from: 1,
        episode_to: None,
    };
    Ok(())
}
