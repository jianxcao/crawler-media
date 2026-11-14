use std::time::{Duration, Instant, SystemTime};

use media::{CatalogRequest, TmdbError};
use ureq::{Body, config::IpFamily, http::Response};

use super::TmdbHttp;

const TOTAL_BUDGET: Duration = media::TMDB_REQUEST_TIMEOUT;
const BODY_LIMIT: u64 = 10 * 1024 * 1024;

struct Failure {
    error: ureq::Error,
    retry_after: Option<Duration>,
}

impl From<ureq::Error> for Failure {
    fn from(error: ureq::Error) -> Self {
        Self {
            error,
            retry_after: None,
        }
    }
}

pub(super) trait Runtime: Send + Sync {
    fn now(&self) -> Instant;
    fn wall_now(&self) -> SystemTime;
    fn jitter(&self, ceiling: Duration) -> Duration;
    fn sleep(&self, delay: Duration);
}

pub(super) struct SystemRuntime;

impl Runtime for SystemRuntime {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn wall_now(&self) -> SystemTime {
        SystemTime::now()
    }
    fn jitter(&self, ceiling: Duration) -> Duration {
        Duration::from_nanos((uuid::Uuid::new_v4().as_u128() % (ceiling.as_nanos() + 1)) as u64)
    }
    fn sleep(&self, delay: Duration) {
        std::thread::sleep(delay);
    }
}

pub(super) fn get(
    client: &TmdbHttp,
    path: &str,
    url: &str,
    proxy: Option<&str>,
) -> Result<String, TmdbError> {
    let runtime = client.runtime.as_ref();
    let request = CatalogRequest::current();
    let deadline = request
        .as_ref()
        .map_or(runtime.now() + TOTAL_BUDGET, |request| {
            request.deadline().min(runtime.now() + TOTAL_BUDGET)
        });
    let mut attempt = 0;
    loop {
        check(runtime, request.as_ref(), deadline)?;
        attempt += 1;
        let family = if attempt % 2 == 1 {
            IpFamily::Ipv4Only
        } else {
            IpFamily::Ipv6Only
        };
        let result = client
            .transport
            .send(
                url,
                proxy,
                family,
                deadline.saturating_duration_since(runtime.now()),
            )
            .map_err(Failure::from)
            .and_then(|response| body(response, runtime.wall_now()));
        check(runtime, request.as_ref(), deadline)?;
        let failure = match result {
            Ok(body) => return Ok(body),
            Err(failure) => failure,
        };
        let status = match &failure.error {
            ureq::Error::StatusCode(status) => *status,
            _ => 0,
        };
        if attempt >= 3 || !transient(&failure.error) {
            tracing::warn!(
                path,
                attempt,
                status,
                "TMDB GET failed without further retry"
            );
            return Err(map_error(path, failure.error));
        }
        let delay = Duration::from_millis(100 * (1 << (attempt - 1)))
            + runtime
                .jitter(Duration::from_millis(50))
                .min(Duration::from_millis(50));
        let delay = delay.max(failure.retry_after.unwrap_or_default());
        if delay >= deadline.saturating_duration_since(runtime.now()) {
            return Err(map_error(path, failure.error));
        }
        tracing::warn!(
            path,
            attempt,
            status,
            delay_ms = delay.as_millis() as u64,
            "Retrying transient TMDB GET within request budget"
        );
        let wake = runtime.now() + delay;
        while runtime.now() < wake {
            check(runtime, request.as_ref(), deadline)?;
            runtime.sleep(
                wake.saturating_duration_since(runtime.now())
                    .min(Duration::from_millis(10)),
            );
        }
    }
}

fn check(
    runtime: &dyn Runtime,
    request: Option<&CatalogRequest>,
    deadline: Instant,
) -> Result<(), TmdbError> {
    if let Some(request) = request {
        request.check_at(runtime.now())?;
    }
    if runtime.now() >= deadline {
        return Err(TmdbError::OriginTimeout);
    }
    Ok(())
}

fn body(response: Response<Body>, wall_now: SystemTime) -> Result<String, Failure> {
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| {
                value
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .map(Duration::from_secs)
                    .or_else(|| {
                        httpdate::parse_http_date(value)
                            .ok()
                            .map(|at| at.duration_since(wall_now).unwrap_or_default())
                    })
            });
        return Err(Failure {
            error: ureq::Error::StatusCode(status),
            retry_after,
        });
    }
    response
        .into_body()
        .into_with_config()
        .limit(BODY_LIMIT)
        .lossy_utf8(false)
        .read_to_string()
        .map_err(Into::into)
}

fn transient(error: &ureq::Error) -> bool {
    match error {
        ureq::Error::StatusCode(429 | 500 | 502 | 503 | 504)
        | ureq::Error::Timeout(_)
        | ureq::Error::ConnectionFailed
        | ureq::Error::HostNotFound => true,
        ureq::Error::Io(error) => {
            if let Some(inner) = error
                .get_ref()
                .and_then(|cause| cause.downcast_ref::<ureq::Error>())
            {
                return transient(inner);
            }
            matches!(
                error.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::NotConnected
            )
        }
        _ => false,
    }
}

fn map_error(path: &str, error: ureq::Error) -> TmdbError {
    match error {
        ureq::Error::StatusCode(404) => TmdbError::NotFound(path.to_owned()),
        error => TmdbError::Http(error.to_string()),
    }
}
