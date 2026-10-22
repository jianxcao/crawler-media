use std::collections::HashMap;
use std::sync::Arc;
use api::cli::Transport;
use api::router;
use axum::body::{Body, to_bytes};
use axum::http::Request;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;
use super::common::*;

pub(super) struct RouterTransport {
    pub(super) app: axum::Router,
    pub(super) token: String,
}

impl Transport for RouterTransport {
    async fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<(u16, Value), String> {
        let mut builder = Request::builder().method(method).uri(path);
        builder = builder.header("authorization", format!("Bearer {}", self.token));
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        let request = builder
            .body(Body::from(
                body.map(|value| value.to_string()).unwrap_or_default(),
            ))
            .map_err(|err| err.to_string())?;
        let response = self
            .app
            .clone()
            .oneshot(request)
            .await
            .map_err(|err| err.to_string())?;
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .map_err(|err| err.to_string())?;
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        let value = match value {
            Value::Object(mut map) if map.get("ok").and_then(Value::as_bool) == Some(true) => {
                map.remove("data").unwrap_or(Value::Null)
            }
            other => other,
        };
        Ok((status, value))
    }
}

pub(super) fn transport(tmp: &tempfile::TempDir) -> RouterTransport {
    search_transport(tmp, HashMap::new())
}

fn search_transport(
    tmp: &tempfile::TempDir,
    bodies: HashMap<&'static str, &'static str>,
) -> RouterTransport {
    RouterTransport {
        app: router(state(
            tmp.path(),
            Arc::new(Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies,
            }),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )),
        token: "management-secret".into(),
    }
}

mod parse;
mod sites_search_admit_catalog;
mod subscribe_filters;
mod users_jobs_downloads;
