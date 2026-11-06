use std::collections::VecDeque;
use std::io::{self, Read};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use ureq::config::IpFamily;

use super::super::{TmdbHttp, retry::Runtime, transport::Transport};

pub struct Clock {
    start: Instant,
    elapsed: Mutex<Duration>,
}

impl Clock {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            elapsed: Mutex::new(Duration::ZERO),
        }
    }

    fn advance(&self, delay: Duration) {
        *self.elapsed.lock().unwrap() += delay;
    }

    fn elapsed(&self) -> Duration {
        *self.elapsed.lock().unwrap()
    }
}

impl Runtime for Clock {
    fn now(&self) -> Instant {
        self.start + self.elapsed()
    }

    fn wall_now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_445_412_480) + self.elapsed()
    }

    fn jitter(&self, ceiling: Duration) -> Duration {
        ceiling / 2
    }

    fn sleep(&self, delay: Duration) {
        self.advance(delay);
    }
}

pub enum Reply {
    Status(u16, Option<&'static str>, Vec<u8>),
    Error(ureq::Error),
    Eof,
    DelayedEof(Duration),
}

pub struct Step {
    pub reply: Reply,
    pub earliest: Duration,
    pub duration: Duration,
    latest: Option<Duration>,
    family: Option<IpFamily>,
}

impl Step {
    pub fn status(status: u16) -> Self {
        Self::new(Reply::Status(status, None, b"success".to_vec()))
    }

    pub fn rate_limit(header: Option<&'static str>) -> Self {
        Self::new(Reply::Status(429, header, Vec::new()))
    }

    pub fn new(reply: Reply) -> Self {
        Self {
            reply,
            earliest: Duration::ZERO,
            duration: Duration::ZERO,
            latest: None,
            family: None,
        }
    }

    pub fn after(mut self, earliest: Duration) -> Self {
        self.earliest = earliest;
        self
    }

    pub fn taking(mut self, duration: Duration) -> Self {
        self.duration = duration;
        self
    }

    pub fn family(mut self, family: IpFamily) -> Self {
        self.family = Some(family);
        self
    }

    pub fn window(mut self, earliest: Duration, latest: Duration) -> Self {
        self.earliest = earliest;
        self.latest = Some(latest);
        self
    }
}

struct Script {
    steps: Mutex<VecDeque<Step>>,
    clock: Arc<Clock>,
}

impl Transport for Script {
    fn send(
        &self,
        _url: &str,
        _proxy: Option<&str>,
        family: IpFamily,
        timeout: Duration,
    ) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        let step = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected GET");
        if step.family.is_some_and(|expected| family != expected) {
            return Ok(response(
                418,
                None,
                ureq::Body::builder().data("wrong family"),
            ));
        }
        let elapsed = self.clock.elapsed();
        if elapsed < step.earliest || step.latest.is_some_and(|latest| elapsed > latest) {
            return Ok(response(
                425,
                None,
                ureq::Body::builder().data("wrong time"),
            ));
        }
        if step.duration >= timeout {
            self.clock.advance(timeout);
            return Err(ureq::Error::Timeout(ureq::Timeout::Global));
        }
        self.clock.advance(step.duration);
        match step.reply {
            Reply::Status(status, retry_after, bytes) => Ok(response(
                status,
                retry_after,
                ureq::Body::builder().data(bytes),
            )),
            Reply::Error(error) => Err(error),
            Reply::Eof => Ok(response(
                200,
                None,
                ureq::Body::builder().reader(PartialEof(false)),
            )),
            Reply::DelayedEof(delay) => Ok(response(
                200,
                None,
                ureq::Body::builder().reader(DelayedEof {
                    clock: self.clock.clone(),
                    delay,
                }),
            )),
        }
    }
}

fn response(
    status: u16,
    retry_after: Option<&str>,
    body: ureq::Body,
) -> ureq::http::Response<ureq::Body> {
    let mut builder = ureq::http::Response::builder().status(status);
    if let Some(value) = retry_after {
        builder = builder.header("Retry-After", value);
    }
    builder.body(body).unwrap()
}

struct PartialEof(bool);

struct DelayedEof {
    clock: Arc<Clock>,
    delay: Duration,
}

impl Read for DelayedEof {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        self.clock.advance(self.delay);
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "truncated body",
        ))
    }
}

impl Read for PartialEof {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.0 {
            self.0 = true;
            buf[0] = b'x';
            return Ok(1);
        }
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "truncated body",
        ))
    }
}

pub struct Fixture {
    _dir: tempfile::TempDir,
    pub http: TmdbHttp,
}

impl Fixture {
    pub fn new(steps: Vec<Step>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::Store::open(dir.path().join("data")).unwrap());
        store
            .put_setting(crate::settings_keys::TMDB_API_KEY, "test-key")
            .unwrap();
        let clock = Arc::new(Clock::new());
        let transport = Arc::new(Script {
            steps: Mutex::new(steps.into()),
            clock: clock.clone(),
        });
        Self {
            _dir: dir,
            http: TmdbHttp {
                store,
                transport,
                runtime: clock,
            },
        }
    }
}
