use std::time::Duration;

use ureq::{Body, config::IpFamily, http::Response};

use crate::http_agent;

pub(super) trait Transport: Send + Sync {
    fn send(
        &self,
        url: &str,
        proxy: Option<&str>,
        family: IpFamily,
        timeout: Duration,
    ) -> Result<Response<Body>, ureq::Error>;
}

pub(super) struct UreqTransport;

impl Transport for UreqTransport {
    fn send(
        &self,
        url: &str,
        proxy: Option<&str>,
        family: IpFamily,
        timeout: Duration,
    ) -> Result<Response<Body>, ureq::Error> {
        http_agent::catalog_agent(proxy, family)
            .get(url)
            .config()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .max_redirects(0)
            .build()
            .call()
    }
}
