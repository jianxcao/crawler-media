use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::IndexerError;
use crate::fetch::{FetchRequest, Fetcher};

pub trait PageSession: Send + Sync {
    fn goto(&self, url: &str) -> Result<(), IndexerError>;
    fn set_cookie_header(&self, header: &str) -> Result<(), IndexerError>;
    fn content(&self) -> Result<String, IndexerError>;
    fn cookie_header(&self) -> Result<String, IndexerError> {
        Ok("session=browser".into())
    }
}

type Opener = Arc<dyn Fn(Option<&str>) -> Result<Arc<dyn PageSession>, IndexerError> + Send + Sync>;

#[derive(Clone, Debug)]
pub struct BrowserConfig {
    pub enabled: bool,
    pub headless: bool,
    pub obscura_enabled: bool,
    pub obscura_url: Option<String>,
    chromium_dir: Option<PathBuf>,
}

impl BrowserConfig {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            headless: true,
            obscura_enabled: false,
            obscura_url: None,
            chromium_dir: None,
        }
    }

    pub fn enable_in(data_dir: &Path) -> Result<Self, IndexerError> {
        let chromium_dir = data_dir.join("chromium");
        fs::create_dir_all(&chromium_dir)?;
        let marker = chromium_dir.join("HEADLESS");
        if !marker.exists() {
            fs::write(&marker, "1")?;
        }
        Ok(Self {
            enabled: true,
            headless: true,
            obscura_enabled: false,
            obscura_url: None,
            chromium_dir: Some(chromium_dir),
        })
    }

    pub fn with_obscura(mut self, enabled: bool, url: Option<String>) -> Self {
        self.obscura_enabled = enabled;
        self.obscura_url = url;
        self
    }

    pub fn chromium_present(&self) -> bool {
        self.chromium_dir.as_ref().is_some_and(|dir| dir.is_dir())
    }

    pub fn chromium_path(&self) -> &Path {
        self.chromium_dir
            .as_deref()
            .unwrap_or_else(|| Path::new(""))
    }
}

pub struct Browser {
    config: BrowserConfig,
    opener: Option<Opener>,
}

impl Browser {
    pub fn new(config: BrowserConfig) -> Self {
        Self {
            config,
            opener: None,
        }
    }

    pub fn with_opener<F>(config: BrowserConfig, opener: F) -> Self
    where
        F: Fn(Option<&str>) -> Result<Arc<dyn PageSession>, IndexerError> + Send + Sync + 'static,
    {
        Self {
            config,
            opener: Some(Arc::new(opener)),
        }
    }

    pub fn config(&self) -> &BrowserConfig {
        &self.config
    }

    fn open(&self, cdp_url: Option<&str>) -> Result<Arc<dyn PageSession>, IndexerError> {
        if let Some(opener) = &self.opener {
            return opener(cdp_url);
        }
        // 如果开启了 Obscura 引擎且配置了 obscura_url，优先作为防检测 CDP 接入
        let target_cdp = if self.config.obscura_enabled && self.config.obscura_url.is_some() {
            self.config.obscura_url.as_deref()
        } else {
            cdp_url
        };
        if let Some(url) = target_cdp {
            let session = crate::cdp_page::open_cdp_session(url)?;
            return Ok(Arc::new(session) as Arc<dyn PageSession>);
        }
        if !self.config.enabled {
            return Err(IndexerError::Fetch(
                "Browser render requested but Browser is disabled".into(),
            ));
        }
        Err(IndexerError::Fetch(
            "managed Chromium launch is not wired in tests; enable Browser and provide cdp_url or an opener".into(),
        ))
    }

    pub fn fetch_html(&self, request: &FetchRequest) -> Result<String, IndexerError> {
        let page = self.open(request.cdp_url.as_deref())?;
        if let Some(cookie) = &request.cookie {
            page.set_cookie_header(cookie)?;
        }
        page.goto(&request.url)?;
        page.content()
    }
}

pub struct RoutedFetcher<H> {
    http: H,
    browser: Browser,
}

impl<H: Fetcher> RoutedFetcher<H> {
    pub fn new(http: H, browser: Browser) -> Self {
        Self { http, browser }
    }
}

impl<H: Fetcher> Fetcher for RoutedFetcher<H> {
    fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
        if request.render {
            self.browser.fetch_html(request)
        } else {
            self.http.fetch(request)
        }
    }
}

pub struct RecordingFetcher {
    bodies: std::collections::HashMap<String, Result<String, String>>,
    requests: Arc<std::sync::Mutex<Vec<FetchRequest>>>,
}

impl RecordingFetcher {
    pub fn new(
        bodies: std::collections::HashMap<String, Result<String, String>>,
        requests: Arc<std::sync::Mutex<Vec<FetchRequest>>>,
    ) -> Self {
        Self { bodies, requests }
    }
}

impl Fetcher for RecordingFetcher {
    fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
        self.requests
            .lock()
            .expect("request log")
            .push(FetchRequest {
                key: request.key.clone(),
                url: request.url.clone(),
                method: request.method,
                body: request.body.clone(),
                cookie: request.cookie.clone(),
                api_key: request.api_key.clone(),
                proxy: request.proxy.clone(),
                render: request.render,
                cdp_url: request.cdp_url.clone(),
            });
        match self.bodies.get(&request.key) {
            Some(Ok(body)) => Ok(body.clone()),
            Some(Err(msg)) => Err(IndexerError::Fetch(msg.clone())),
            None => Err(IndexerError::Fetch(format!("missing {}", request.key))),
        }
    }
}
