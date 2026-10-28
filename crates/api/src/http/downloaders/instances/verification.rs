//! Verification observes one connection configuration and only publishes against it.
use super::*;
use crate::http::store_wait::{busy_response, store_read, store_write};

pub(crate) async fn verify_downloader(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let id = match DownloaderId::from_str(&id) {
        Ok(id) => id,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "downloader.invalid",
                "下载器 id 无效",
            );
        }
    };
    let row = match store_read(&state, move |store| store.get_downloader(id).ok().flatten()).await {
        Ok(Some(row)) => row,
        Ok(None) => return err(StatusCode::NOT_FOUND, "downloader.missing", "下载器不存在"),
        Err(error) => return error.response(),
    };
    if !matches!(row.kind.as_str(), "qbittorrent" | "transmission") {
        return err(
            StatusCode::BAD_REQUEST,
            "downloader.invalid",
            "未知下载器类型",
        );
    }
    let connection = row.clone();
    let result = match state
        .http_work
        .run_to_completion(move || verify_connection(connection))
        .await
    {
        Ok(result) => result,
        Err(crate::blocking_budget::BlockingError::Busy) => return busy_response(),
        Err(error) => Err(error.to_string()),
    };
    if let Err(error) = &result {
        tracing::error!(%id, %error, "下载器验证连接失败");
    }
    store_write(&state, move |store| {
        let Some(_) = store.get_downloader(id).ok().flatten() else {
            return err(StatusCode::NOT_FOUND, "downloader.missing", "下载器不存在");
        };
        let outcome = result.as_ref().map(|_| ()).map_err(String::as_str);
        match store.record_downloader_verification(&row, outcome) {
            Ok(false) => {
                tracing::warn!(%id, "下载器配置在验证期间改变，丢弃过期结果");
                err(
                    StatusCode::CONFLICT,
                    "downloader.changed",
                    "下载器配置已改变，请重新验证",
                )
            }
            Err(error) => {
                tracing::error!(%id, %error, "下载器验证结果写入失败，事务已回滚");
                err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                )
            }
            Ok(true) => match result {
                Ok(()) => ok(json!({ "ok": true, "error": null })).into_response(),
                Err(error) => ok(json!({ "ok": false, "error": error })).into_response(),
            },
        }
    })
    .await
    .unwrap_or_else(|response| response)
}

fn verify_connection(row: store::DownloaderRow) -> Result<(), String> {
    match row.kind.as_str() {
        "qbittorrent" => downloader::QbitDownloader::connect(downloader::QbitConfig {
            url: row.url,
            username: row.username.unwrap_or_default(),
            password: row.password.unwrap_or_default(),
            category: row.category,
            path_maps: row.path_maps,
        })
        .map(|_| ())
        .map_err(|error| error.to_string()),
        "transmission" => {
            downloader::TransmissionDownloader::connect(downloader::TransmissionConfig {
                url: row.url,
                username: row.username,
                password: row.password,
                path_maps: row.path_maps,
            })
            .map(|_| ())
            .map_err(|error| error.to_string())
        }
        _ => Err("未知下载器类型".into()),
    }
}
