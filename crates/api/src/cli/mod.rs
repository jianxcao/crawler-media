mod actions;
mod catalog;
mod directory;
mod downloaders;
mod filters;
mod lists;
mod parse;
mod sites;
mod users;

use serde_json::Value;

pub use filters::FilterAtomSpec;
pub use parse::Command;
pub struct HttpTransport {
    base: String,
    token: String,
}

impl HttpTransport {
    pub fn from_env() -> Result<Self, String> {
        let base = std::env::var("CRAWLER_MEDIA_URL")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "http://127.0.0.1:18765".into());
        let token = std::env::var("CRAWLER_MEDIA_TOKEN")
            .map_err(|_| "CRAWLER_MEDIA_TOKEN is required".to_string())?;
        Ok(Self { base, token })
    }
}

impl Transport for HttpTransport {
    async fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<(u16, Value), String> {
        let url = format!("{}{path}", self.base.trim_end_matches('/'));
        let agent = ureq::Agent::new_with_defaults();
        let auth = format!("Bearer {}", self.token);
        let result = match (method, body) {
            ("GET", None) => agent.get(&url).header("authorization", &auth).call(),
            ("POST", None) => agent.post(&url).header("authorization", &auth).send(""),
            ("POST", Some(body)) => agent
                .post(&url)
                .header("authorization", &auth)
                .header("content-type", "application/json")
                .send(body.to_string()),
            ("PUT", Some(body)) => agent
                .put(&url)
                .header("authorization", &auth)
                .header("content-type", "application/json")
                .send(body.to_string()),
            ("PATCH", Some(body)) => agent
                .patch(&url)
                .header("authorization", &auth)
                .header("content-type", "application/json")
                .send(body.to_string()),
            ("DELETE", None) => agent.delete(&url).header("authorization", &auth).call(),
            _ => return Err(format!("unsupported CLI method {method}")),
        };
        match result {
            Ok(resp) => {
                let (status, body) = read_response(resp)?;
                Ok((status, unwrap_data(body)))
            }
            Err(error) => Err(error.to_string()),
        }
    }
}

fn read_response(resp: ureq::http::Response<ureq::Body>) -> Result<(u16, Value), String> {
    let status = resp.status().as_u16();
    let text = resp
        .into_body()
        .read_to_string()
        .map_err(|err| err.to_string())?;
    let value = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::Null)
    };
    Ok((status, value))
}

pub trait Transport {
    fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> impl Future<Output = Result<(u16, Value), String>> + Send;
}

/// v1 信封：成功体在 `data`。非对象或没有 `data` 时原样返回。
fn unwrap_data(body: Value) -> Value {
    match body {
        Value::Object(mut map) if map.get("ok").and_then(Value::as_bool) == Some(true) => {
            map.remove("data").unwrap_or(Value::Null)
        }
        other => other,
    }
}

pub async fn run(command: Command, transport: &impl Transport) -> Result<String, String> {
    match command {
        Command::Serve => Err("serve is the process, not a CLI request".into()),
        Command::Sites => lists::list_sites(transport).await,
        Command::SitesAdd {
            name,
            url,
            profile_id,
            cookie,
            api_key,
        } => {
            actions::create_site(
                transport,
                &name,
                &url,
                &profile_id,
                cookie.as_deref(),
                api_key.as_deref(),
            )
            .await
        }
        Command::SitesDisable { id } => actions::disable_site(transport, &id).await,
        Command::SitesEnable { id } => actions::enable_site(transport, &id).await,
        Command::Subscribe { title, kind } => {
            actions::create_subscribe(transport, &title, &kind).await
        }
        Command::Subscribes => lists::list_subscribes(transport).await,
        Command::Filters => lists::list_filters(transport).await,
        Command::FiltersAdd { name, atoms } => {
            filters::create_filter(transport, &name, &atoms).await
        }
        Command::FiltersDefault { id } => filters::set_default_filter(transport, &id).await,
        Command::Users => lists::list_users(transport).await,
        Command::UsersAdd { login, token } => users::create_user(transport, &login, &token).await,
        Command::Jobs => lists::list_jobs(transport).await,
        Command::JobsTick { now } => actions::tick_jobs(transport, now).await,
        Command::Search { query } => actions::search_torrents(transport, &query).await,
        Command::CatalogSearch { query } => catalog::search_catalog(transport, &query).await,
        Command::Catalog => catalog::list_catalog_cache(transport).await,
        Command::CatalogDelete { source, cache_key } => {
            catalog::delete_catalog_cache(transport, &source, &cache_key).await
        }
        Command::Admit {
            subscribe_id,
            enclosure,
        } => actions::admit_torrent(transport, &subscribe_id, &enclosure).await,
        Command::Library => lists::list_library(transport).await,
        Command::Ledger => lists::list_ledger(transport).await,
        Command::Directory => lists::list_directory(transport).await,
        Command::DirectoryAddRoot { kind, path } => {
            actions::add_library_root(transport, &kind, &path).await
        }
        Command::DirectoryRemoveRoot { id } => actions::remove_library_root(transport, &id).await,
        Command::DirectoryWatchInplace { path } => {
            actions::set_watch_inplace(transport, &path).await
        }
        Command::DirectoryWatchIntake { path } => actions::set_watch_intake(transport, &path).await,
        Command::DirectoryTransferMode { mode } => {
            actions::set_transfer_mode(transport, &mode).await
        }
        Command::DirectoryScrape { enabled } => actions::set_scrape(transport, enabled).await,
        Command::DirectoryMovieNaming { pattern } => {
            actions::set_movie_naming(transport, &pattern).await
        }
        Command::DirectoryTvNaming { pattern } => actions::set_tv_naming(transport, &pattern).await,
        Command::Downloaders => lists::list_downloaders(transport).await,
        Command::DownloadersAdd {
            name,
            kind,
            url,
            username,
            password,
            is_default,
        } => {
            downloaders::create_downloader(
                transport,
                &name,
                &kind,
                &url,
                username.as_deref(),
                password.as_deref(),
                is_default,
            )
            .await
        }
        Command::DownloadersDefault { id } => {
            downloaders::set_default_downloader(transport, &id).await
        }
        Command::Downloads => lists::list_downloads(transport).await,
        Command::Unidentified => lists::list_unidentified(transport).await,
        Command::ClaimUnidentified {
            path,
            title,
            kind,
            year,
        } => actions::claim_unidentified(transport, &path, &title, &kind, year).await,
    }
}
