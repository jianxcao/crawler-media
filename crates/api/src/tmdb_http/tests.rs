mod cache;
mod cancellation;
mod support;
mod wire;

use std::io;
use std::time::Duration;

use media::{CatalogGet, TmdbError};
use ureq::config::IpFamily;

use self::support::{Fixture, Reply, Step};

const PATH: &str = "/movie/123";

#[test]
fn transient_statuses_recover_within_three_attempts() {
    let fixture = Fixture::new(vec![
        Step::status(500),
        Step::status(502),
        Step::status(200),
    ]);
    assert_eq!(fixture.http.get(PATH).unwrap(), "success");
    for status in [500, 502, 503, 504] {
        let fixture = Fixture::new(vec![Step::status(status), Step::status(200)]);
        assert_eq!(
            fixture.http.get(PATH).unwrap(),
            "success",
            "status {status}"
        );
    }
}

#[test]
fn three_attempts_exhaust_budget_and_preserve_last_error() {
    let fixture = Fixture::new(vec![
        Step::status(500),
        Step::status(502),
        Step::status(503),
        Step::status(200),
    ]);
    let error = fixture.http.get(PATH).unwrap_err().to_string();
    assert!(error.contains("503"), "{error}");
}

#[test]
fn permanent_statuses_stop_before_success_and_404_remains_typed() {
    for status in [400, 401, 403, 404, 405, 501, 505] {
        let fixture = Fixture::new(vec![Step::status(status), Step::status(200)]);
        let error = fixture.http.get(PATH).unwrap_err();
        if status == 404 {
            assert!(matches!(error, TmdbError::NotFound(ref path) if path == PATH));
        } else {
            assert!(
                matches!(error, TmdbError::Http(_)),
                "status {status}: {error}"
            );
        }
    }
}

#[test]
fn connection_timeout_and_truncated_body_are_retried() {
    for error in [
        ureq::Error::ConnectionFailed,
        ureq::Error::Timeout(ureq::Timeout::Connect),
        ureq::Error::Io(io::Error::new(io::ErrorKind::ConnectionReset, "reset")),
    ] {
        let fixture = Fixture::new(vec![Step::new(Reply::Error(error)), Step::status(200)]);
        assert_eq!(fixture.http.get(PATH).unwrap(), "success");
    }
    let fixture = Fixture::new(vec![Step::new(Reply::Eof), Step::status(200)]);
    assert_eq!(fixture.http.get(PATH).unwrap(), "success");
}

#[test]
fn invalid_body_or_certificate_error_is_terminal() {
    let fixture = Fixture::new(vec![
        Step::new(Reply::Status(200, None, vec![0xff])),
        Step::status(200),
    ]);
    assert!(fixture.http.get(PATH).is_err());

    let fixture = Fixture::new(vec![
        Step::new(Reply::Error(ureq::Error::Tls("invalid certificate"))),
        Step::status(200),
    ]);
    assert!(fixture.http.get(PATH).is_err());

    let fixture = Fixture::new(vec![
        Step::new(Reply::Status(200, None, vec![b'x'; 10 * 1024 * 1024 + 1])),
        Step::status(200),
    ]);
    assert!(fixture.http.get(PATH).is_err());
}

#[test]
fn retry_after_delta_and_http_date_are_minimum_delays() {
    for header in ["2", "Wed, 21 Oct 2015 07:28:02 GMT"] {
        let fixture = Fixture::new(vec![
            Step::rate_limit(Some(header)),
            Step::status(200).after(Duration::from_secs(2)),
        ]);
        assert_eq!(fixture.http.get(PATH).unwrap(), "success", "{header}");
    }
}

#[test]
fn retry_after_past_deadline_does_not_start_another_get() {
    let fixture = Fixture::new(vec![Step::rate_limit(Some("7")), Step::status(200)]);
    assert!(fixture.http.get(PATH).is_err());
}

#[test]
fn missing_and_invalid_retry_after_use_jittered_backoff() {
    for header in [None, Some("invalid")] {
        let fixture = Fixture::new(vec![
            Step::rate_limit(header),
            Step::status(200).after(Duration::from_nanos(1)),
        ]);
        assert_eq!(fixture.http.get(PATH).unwrap(), "success", "{header:?}");
    }
}

#[test]
fn attempts_share_a_single_six_second_deadline() {
    let fixture = Fixture::new(vec![
        Step::status(500).taking(Duration::from_secs(4)),
        Step::status(200).taking(Duration::from_secs(4)),
    ]);
    assert!(fixture.http.get(PATH).is_err());

    let fixture = Fixture::new(vec![
        Step::new(Reply::DelayedEof(Duration::from_secs(4))),
        Step::status(200).taking(Duration::from_secs(4)),
    ]);
    assert!(fixture.http.get(PATH).is_err());
}

#[test]
fn connection_fallback_shares_the_three_attempt_budget() {
    let fixture = Fixture::new(vec![
        Step::new(Reply::Error(ureq::Error::ConnectionFailed)).family(IpFamily::Ipv4Only),
        Step::new(Reply::Error(ureq::Error::ConnectionFailed)).family(IpFamily::Ipv6Only),
        Step::status(200).family(IpFamily::Ipv4Only),
    ]);
    assert_eq!(fixture.http.get(PATH).unwrap(), "success");
}

#[test]
fn jitter_is_included_in_each_bounded_backoff() {
    let fixture = Fixture::new(vec![
        Step::status(500),
        Step::status(502).window(Duration::from_millis(125), Duration::from_millis(125)),
        Step::status(200).window(Duration::from_millis(350), Duration::from_millis(350)),
    ]);
    assert_eq!(fixture.http.get(PATH).unwrap(), "success");
}
