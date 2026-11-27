//! Rule-sets: named Filter groups of atomic rules (CONTEXT.md **Filter**).

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{AtomRule, Filter, FilterAtom, FilterId, UnknownHrPolicy};
use serde::Deserialize;
use serde_json::{Value, json};
use std::str::FromStr;

use crate::http::strict_json::StrictJson;
use crate::http::{err, ok, ok_list};
use crate::management::ApiState;
use crate::settings_keys;
use store::StoreError;

#[derive(Deserialize)]
pub(crate) struct RuleSetInput {
    name: String,
    atoms: Vec<AtomInput>,
    #[serde(default)]
    keep_old_versions: Option<bool>,
}

#[derive(Deserialize)]
pub(crate) struct AtomInput {
    kind: String,
    #[serde(default)]
    value: Option<String>,
    priority: i32,
    /// 当前原子命中时排除该 Torrent。
    #[serde(default)]
    exclude: bool,
    #[serde(default)]
    unknown: Option<UnknownHrPolicy>,
}

fn atom_from_input(input: AtomInput) -> Result<FilterAtom, String> {
    let value = || {
        input
            .value
            .clone()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("{} atom 需要 value", input.kind))
    };
    let rule = match input.kind.as_str() {
        "resolution" => AtomRule::Resolution(value()?),
        "source" => AtomRule::Source(value()?),
        "free" => AtomRule::Free,
        "hr" => AtomRule::Hr {
            unknown: input
                .unknown
                .ok_or_else(|| "hr atom 需要 unknown（exclude/allow）".to_string())?,
        },
        "video_codec" => AtomRule::Codec(value()?),
        "title_match" => AtomRule::TitleMatch(value()?),
        "platform" => AtomRule::Platform(value()?),
        "team" => AtomRule::Team(value()?),
        "hdr" => AtomRule::Hdr(value()?),
        "size" => parse_size_atom(&input.kind, &input.value)?,
        "min_seeders" => AtomRule::MinSeeders(
            input
                .value
                .as_deref()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| format!("{} atom 需要数字", input.kind))?,
        ),
        "subtitle_language" => AtomRule::SubtitleLanguage(value()?),
        "audio_language" => AtomRule::AudioLanguage(value()?),
        "site" => AtomRule::Site(value()?),
        "wash_target" => AtomRule::WashTarget(value()?),
        "upgrade_ladder" => {
            let ladder = value()?;
            // L3: only the dimensions the chooser understands are accepted.
            if let Some(bad) = subscribe::unknown_ladder_dimension(&ladder) {
                return Err(format!(
                    "upgrade_ladder 维度 {bad} 无效（可选 {}）",
                    subscribe::LADDER_DIMENSIONS.join("/")
                ));
            }
            AtomRule::UpgradeLadder(ladder)
        }
        other => return Err(format!("未知的 Filter atom {other}")),
    };
    Ok(FilterAtom {
        priority: input.priority,
        rule,
        exclude: input.exclude,
    })
}

/// `size` 原子：value 形如 `{min_mb}-{max_mb}`（单边可为空，0 表示不设限）。
fn parse_size_atom(kind: &str, value: &Option<String>) -> Result<AtomRule, String> {
    let raw = value
        .as_deref()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("{kind} atom 需要 value"))?;
    let (min, max) = raw
        .split_once('-')
        .ok_or_else(|| format!("{kind} atom 值须为 {{min}}-{{max}}"))?;
    Ok(AtomRule::Size {
        min_mb: parse_opt(min),
        max_mb: parse_opt(max),
    })
}

fn parse_opt(raw: &str) -> Option<u64> {
    raw.trim().parse().ok().filter(|n| *n > 0)
}

fn atom_json(atom: &FilterAtom) -> Value {
    let (kind, value): (&str, Option<String>) = match &atom.rule {
        AtomRule::Resolution(v) => ("resolution", Some(v.clone())),
        AtomRule::Source(v) => ("source", Some(v.clone())),
        AtomRule::Free => ("free", None),
        AtomRule::Hr { .. } => ("hr", None),
        AtomRule::Codec(v) => ("video_codec", Some(v.clone())),
        AtomRule::TitleMatch(v) => ("title_match", Some(v.clone())),
        AtomRule::Platform(v) => ("platform", Some(v.clone())),
        AtomRule::Team(v) => ("team", Some(v.clone())),
        AtomRule::Hdr(v) => ("hdr", Some(v.clone())),
        AtomRule::Size { min_mb, max_mb } => (
            "size",
            Some(format!(
                "{}-{}",
                min_mb.unwrap_or(0),
                max_mb.map_or(String::new(), |m| m.to_string())
            )),
        ),
        AtomRule::MinSeeders(n) => ("min_seeders", Some(n.to_string())),
        AtomRule::SubtitleLanguage(v) => ("subtitle_language", Some(v.clone())),
        AtomRule::AudioLanguage(v) => ("audio_language", Some(v.clone())),
        AtomRule::Site(v) => ("site", Some(v.clone())),
        AtomRule::WashTarget(v) => ("wash_target", Some(v.clone())),
        AtomRule::UpgradeLadder(v) => ("upgrade_ladder", Some(v.clone())),
    };
    let mut value = json!({
        "kind": kind,
        "value": value,
        "priority": atom.priority,
        "exclude": atom.exclude,
    });
    if let AtomRule::Hr { unknown } = atom.rule {
        value["unknown"] = json!(unknown);
    }
    value
}

fn rule_set_json(filter: &Filter, is_default: bool) -> Value {
    json!({
        "id": filter.id.to_string(),
        "name": filter.name,
        "is_default": is_default,
        "keep_old_versions": filter.keep_old_versions,
        "atoms": filter.atoms.iter().map(atom_json).collect::<Vec<_>>(),
    })
}

pub(crate) async fn list_rule_sets(State(state): State<ApiState>) -> Response {
    super::store_wait::store_read(&state, &[store::Db::App], move |store| {
        let filters = match store.list_filters() {
            Ok(filters) => filters,
            Err(StoreError::Invalid(message)) => {
                return err(StatusCode::BAD_REQUEST, "rule_set.invalid", &message);
            }
            Err(error) => return rule_set_read_error(error),
        };
        let default = match store.get_setting(settings_keys::DEFAULT_FILTER_ID) {
            Ok(raw) => raw.and_then(|id| FilterId::from_str(&id).ok()),
            Err(error) => return rule_set_read_error(error),
        };
        ok_list(
            filters
                .iter()
                .map(|f| rule_set_json(f, default == Some(f.id)))
                .collect(),
        )
        .into_response()
    })
    .await
    .unwrap_or_else(|error| error.response())
}

pub(crate) async fn create_rule_set(
    State(state): State<ApiState>,
    StrictJson(body): StrictJson<RuleSetInput>,
) -> Response {
    if body.name.trim().is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "rule_set.invalid",
            "规则组名称必填",
        );
    }
    let atoms = match body
        .atoms
        .into_iter()
        .map(atom_from_input)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(atoms) if !atoms.is_empty() => atoms,
        Ok(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "rule_set.invalid",
                "规则组至少需要一个 atom",
            );
        }
        Err(message) => return err(StatusCode::BAD_REQUEST, "rule_set.invalid", &message),
    };
    let filter = Filter {
        id: FilterId::new(),
        name: body.name,
        atoms,
        keep_old_versions: body.keep_old_versions.unwrap_or(false),
    };
    super::store_wait::store_write(&state, move |store| {
        if let Err(error) = store.insert_filter(&filter) {
            return rule_set_store_error(error);
        }
        (StatusCode::CREATED, ok(rule_set_json(&filter, false))).into_response()
    })
    .await
    .unwrap_or_else(|response| response)
}

pub(crate) async fn get_rule_set(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let id = match FilterId::from_str(&id) {
        Ok(id) => id,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "rule_set.invalid",
                "规则组 id 无效",
            );
        }
    };
    super::store_wait::store_read(&state, &[store::Db::App], move |store| {
        let filter = match store.get_filter(id) {
            Ok(Some(filter)) => filter,
            Ok(None) => {
                return err(StatusCode::NOT_FOUND, "rule_set.missing", "规则组不存在");
            }
            Err(StoreError::Invalid(message)) => {
                return err(StatusCode::BAD_REQUEST, "rule_set.invalid", &message);
            }
            Err(error) => return rule_set_read_error(error),
        };
        let expected = id.to_string();
        let is_default = match store.get_setting(settings_keys::DEFAULT_FILTER_ID) {
            Ok(raw) => raw.as_deref() == Some(expected.as_str()),
            Err(error) => return rule_set_read_error(error),
        };
        ok(rule_set_json(&filter, is_default)).into_response()
    })
    .await
    .unwrap_or_else(|error| error.response())
}

pub(crate) async fn patch_rule_set(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    StrictJson(body): StrictJson<RuleSetInput>,
) -> Response {
    let id = match FilterId::from_str(&id) {
        Ok(id) => id,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "rule_set.invalid",
                "规则组 id 无效",
            );
        }
    };
    if body.name.trim().is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "rule_set.invalid",
            "规则组名称必填",
        );
    }
    let atoms = match body
        .atoms
        .into_iter()
        .map(atom_from_input)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(atoms) if !atoms.is_empty() => atoms,
        Ok(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "rule_set.invalid",
                "规则组至少需要一个 atom",
            );
        }
        Err(message) => return err(StatusCode::BAD_REQUEST, "rule_set.invalid", &message),
    };
    super::store_wait::store_write(&state, move |store| {
        let existing = match store.get_filter(id) {
            Ok(Some(f)) => f,
            Ok(None) => return err(StatusCode::NOT_FOUND, "rule_set.missing", "规则组不存在"),
            Err(error) => return rule_set_read_error(error),
        };
        let filter = Filter {
            id,
            name: body.name,
            atoms,
            keep_old_versions: body.keep_old_versions.unwrap_or(existing.keep_old_versions),
        };
        if let Err(error) = store.save_filter(&filter) {
            return rule_set_store_error(error);
        }
        let expected = id.to_string();
        let is_default = match store.get_setting(settings_keys::DEFAULT_FILTER_ID) {
            Ok(raw) => raw.as_deref() == Some(expected.as_str()),
            Err(error) => return rule_set_read_error(error),
        };
        ok(rule_set_json(&filter, is_default)).into_response()
    })
    .await
    .unwrap_or_else(|response| response)
}

pub(crate) async fn delete_rule_set(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let id = match FilterId::from_str(&id) {
        Ok(id) => id,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "rule_set.invalid",
                "规则组 id 无效",
            );
        }
    };
    super::store_wait::store_write(&state, move |store| {
        let expected = id.to_string();
        let is_default = match store.get_setting(settings_keys::DEFAULT_FILTER_ID) {
            Ok(raw) => raw.as_deref() == Some(expected.as_str()),
            Err(error) => return rule_set_read_error(error),
        };
        if is_default {
            return err(
                StatusCode::BAD_REQUEST,
                "rule_set.protected",
                "不能删除默认规则组",
            );
        }
        let subscribes = match store.list_all_subscribes() {
            Ok(rows) => rows,
            Err(error) => return rule_set_read_error(error),
        };
        let libraries = match store.list_libraries() {
            Ok(rows) => rows,
            Err(error) => return rule_set_read_error(error),
        };
        let id_str = id.to_string();
        let in_use = subscribes
            .iter()
            .any(|s| s.filter_id == id || s.wash_cut_filter_id == Some(id))
            || libraries
                .iter()
                .any(|lib| lib.default_filter_id.as_deref() == Some(id_str.as_str()));
        if in_use {
            return err(
                StatusCode::CONFLICT,
                "rule_set.in_use",
                "该规则组正被订阅或媒体库引用，无法删除",
            );
        }
        match store.delete_filter(id) {
            Ok(true) => ok(json!({ "deleted": true })).into_response(),
            Ok(false) => err(StatusCode::NOT_FOUND, "rule_set.missing", "规则组不存在"),
            Err(error) => rule_set_store_error(error),
        }
    })
    .await
    .unwrap_or_else(|response| response)
}

fn rule_set_read_error(error: StoreError) -> Response {
    match error {
        StoreError::ReadBusy
        | StoreError::ReadClosed
        | StoreError::ReadDeadline
        | StoreError::ReadCancelled => super::store_wait::busy_response(),
        error => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}

fn rule_set_store_error(error: StoreError) -> Response {
    match error {
        StoreError::WriteBusy
        | StoreError::ReadBusy
        | StoreError::ReadClosed
        | StoreError::ReadDeadline
        | StoreError::ReadCancelled => super::store_wait::busy_response(),
        error => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}

pub(crate) async fn put_default_rule_set(
    State(state): State<ApiState>,
    Json(body): Json<Value>,
) -> Response {
    let raw = body["id"].as_str().unwrap_or_default();
    let id = match FilterId::from_str(raw) {
        Ok(id) => id,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "rule_set.invalid",
                "规则组 id 无效",
            );
        }
    };
    super::store_wait::store_write(&state, move |store| {
        match store.get_filter(id) {
            Ok(Some(_)) => {}
            Ok(None) => return err(StatusCode::NOT_FOUND, "rule_set.missing", "规则组不存在"),
            Err(error) => return rule_set_read_error(error),
        }
        if let Err(error) = store.put_setting(settings_keys::DEFAULT_FILTER_ID, &id.to_string()) {
            return rule_set_store_error(error);
        }
        ok(json!({ "default_rule_set_id": id.to_string() })).into_response()
    })
    .await
    .unwrap_or_else(|response| response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct NoopFetcher;
    impl indexer::Fetcher for NoopFetcher {
        fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
            Err(indexer::IndexerError::Fetch("unused".into()))
        }
    }

    fn rule_set_test_state(tmp: &std::path::Path) -> ApiState {
        ApiState::new(
            crate::Store::open(tmp.join("data")).unwrap(),
            "test-token".into(),
            indexer::RegistrySnapshot::builtin().unwrap(),
            Arc::new(NoopFetcher),
            Arc::new(downloader::MemoryDownloader::new(tmp.join("dl"))),
        )
        .unwrap()
    }

    #[test]
    fn rule_set_read_error_maps_read_closed_to_503() {
        let resp = rule_set_read_error(StoreError::ReadClosed);
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn rule_set_store_error_maps_write_busy_and_read_unavailable_to_503() {
        for err in [
            StoreError::WriteBusy,
            StoreError::ReadBusy,
            StoreError::ReadClosed,
            StoreError::ReadDeadline,
            StoreError::ReadCancelled,
        ] {
            let resp = rule_set_store_error(err);
            assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        }
        let resp = rule_set_store_error(StoreError::Protected("nope".into()));
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn delete_default_rule_set_returns_503_and_preserves_filter_when_app_reader_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let state = rule_set_test_state(tmp.path());
        let filter = Filter::new(FilterId::new(), "Default", vec![]);
        state.store().insert_filter(&filter).unwrap();
        state
            .store()
            .put_setting(settings_keys::DEFAULT_FILTER_ID, &filter.id.to_string())
            .unwrap();

        state.store().reader_pool(store::Db::App).close();
        let resp = delete_rule_set(State(state), Path(filter.id.to_string())).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        let conn = rusqlite::Connection::open(tmp.path().join("data/app.db")).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM filters WHERE id = ?1",
                [filter.id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "default filter must not be deleted when Db::App reader is closed"
        );
    }

    #[tokio::test]
    async fn put_default_rule_set_returns_503_when_app_reader_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let state = rule_set_test_state(tmp.path());
        let filter = Filter::new(FilterId::new(), "Candidate", vec![]);
        state.store().insert_filter(&filter).unwrap();

        state.store().reader_pool(store::Db::App).close();
        let resp =
            put_default_rule_set(State(state), Json(json!({ "id": filter.id.to_string() }))).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
