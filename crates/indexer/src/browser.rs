use std::path::Path;
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

type ConfigProvider = Arc<dyn Fn() -> Result<BrowserConfig, IndexerError> + Send + Sync>;

/// Only external HTTP(S) CDP discovery endpoints are supported. No managed launcher exists.
#[derive(Clone, Debug)]
pub struct BrowserConfig {
    /// Legacy managed flag. Setting this directly is rejected by validation.
    pub enabled: bool,
    pub headless: bool,
    pub obscura_enabled: bool,
    pub obscura_url: Option<String>,
    pub cdp_enabled: bool,
    pub cdp_url: Option<String>,
}

impl BrowserConfig {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            headless: true,
            obscura_enabled: false,
            obscura_url: None,
            cdp_enabled: false,
            cdp_url: None,
        }
    }

    /// Managed Chromium is unavailable; never create directories that masquerade as binaries.
    pub fn enable_in(_data_dir: &Path) -> Result<Self, IndexerError> {
        let error = IndexerError::Fetch(
            "managed Chromium is unsupported; configure an external HTTP(S) CDP endpoint".into(),
        );
        tracing::error!(%error, capability = "managed_chromium", "Browser capability rejected");
        Err(error)
    }

    pub fn with_obscura(mut self, enabled: bool, url: Option<String>) -> Self {
        self.obscura_enabled = enabled;
        self.obscura_url = url;
        self
    }

    pub fn with_cdp(mut self, enabled: bool, url: Option<String>) -> Self {
        self.cdp_enabled = enabled;
        self.cdp_url = url;
        self
    }

    pub fn chromium_present(&self) -> bool {
        false
    }
    pub fn chromium_path(&self) -> &Path {
        Path::new("")
    }

    fn validate_managed(&self) -> Result<(), IndexerError> {
        if self.enabled {
            return Err(IndexerError::Fetch(
                "managed Chromium is unsupported; use external CDP".into(),
            ));
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), IndexerError> {
        self.validate_managed()?;
        if self.obscura_enabled {
            Self::validate_endpoint(self.obscura_url.as_deref())?;
        }
        if self.cdp_enabled {
            Self::validate_endpoint(self.cdp_url.as_deref())?;
        }
        Ok(())
    }

    /// The CDP transport discovers a page target via HTTP /json/new, not a raw WebSocket URL.
    pub fn validate_endpoint(endpoint: Option<&str>) -> Result<(), IndexerError> {
        let valid = endpoint
            .and_then(|url| url.parse::<tungstenite::http::Uri>().ok())
            .is_some_and(|uri| {
                matches!(uri.scheme_str(), Some("http" | "https"))
                    && uri.host().is_some_and(|host| !host.is_empty())
                    && uri.authority().is_some_and(|a| !a.as_str().contains('@'))
                    && uri.path_and_query().is_none_or(|p| p.query().is_none())
            });
        if valid {
            Ok(())
        } else {
            Err(IndexerError::Fetch("Browser requires a valid HTTP(S) CDP discovery URL; empty, credentials, query and ws/wss endpoints are unsupported".into()))
        }
    }

    fn endpoint<'a>(
        &'a self,
        site: Option<&'a str>,
    ) -> Result<(&'a str, &'static str), IndexerError> {
        self.validate_managed()?;
        // An explicit Site endpoint is authoritative, including when a global route is invalid.
        let (endpoint, route) = if let Some(endpoint) = site {
            (Some(endpoint), "site")
        } else {
            self.validate()?;
            if self.obscura_enabled {
                (self.obscura_url.as_deref(), "obscura")
            } else if self.cdp_enabled {
                (self.cdp_url.as_deref(), "global_cdp")
            } else {
                (None, "unconfigured")
            }
        };
        Self::validate_endpoint(endpoint)?;
        Ok((
            endpoint.ok_or_else(|| IndexerError::Fetch("Browser endpoint missing".into()))?,
            route,
        ))
    }
}

pub struct Browser {
    config: BrowserConfig,
    provider: Option<ConfigProvider>,
    opener: Option<Opener>,
}

impl Browser {
    pub fn new(config: BrowserConfig) -> Self {
        Self {
            config,
            provider: None,
            opener: None,
        }
    }

    pub fn with_opener<F>(config: BrowserConfig, opener: F) -> Self
    where
        F: Fn(Option<&str>) -> Result<Arc<dyn PageSession>, IndexerError> + Send + Sync + 'static,
    {
        Self {
            config,
            provider: None,
            opener: Some(Arc::new(opener)),
        }
    }

    /// Read effective configuration at the request boundary. The provider owns persistence IO.
    pub fn with_config_provider<F>(mut self, provider: F) -> Self
    where
        F: Fn() -> Result<BrowserConfig, IndexerError> + Send + Sync + 'static,
    {
        self.provider = Some(Arc::new(provider));
        self
    }

    pub fn config(&self) -> &BrowserConfig {
        &self.config
    }

    fn open(&self, cdp_url: Option<&str>) -> Result<Arc<dyn PageSession>, IndexerError> {
        let config = match &self.provider {
            Some(provider) => provider()?,
            None => self.config.clone(),
        };
        let (endpoint, route) = config.endpoint(cdp_url).inspect_err(|error| {
            tracing::error!(%error, "Browser route rejected");
        })?;
        tracing::debug!(route, "Opening external Browser session");
        // Inject transport only after routing, so tests cannot bypass production decisions.
        let session = match &self.opener {
            Some(opener) => opener(Some(endpoint)),
            None => crate::cdp_page::open_cdp_session(endpoint)
                .map(|session| Arc::new(session) as Arc<dyn PageSession>),
        };
        session.inspect_err(|error| {
            tracing::error!(%error, route, "Browser session open failed");
        })
    }

    pub fn fetch_html(&self, request: &FetchRequest) -> Result<String, IndexerError> {
        let page = self.open(request.cdp_url.as_deref())?;
        let result = (|| {
            if let Some(cookie) = &request.cookie {
                page.set_cookie_header(cookie)?;
            }
            page.goto(&request.url)?;
            page.content()
        })();
        result.inspect_err(|error| {
            tracing::error!(%error, request_key = %request.key, "Browser render failed");
        })
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
