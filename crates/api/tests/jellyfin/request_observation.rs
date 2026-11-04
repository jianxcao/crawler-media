use super::*;
use std::io::Write;
use std::sync::Mutex;

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

struct Writer(Arc<Mutex<Vec<u8>>>);

impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buffer {
    type Writer = Writer;

    fn make_writer(&'a self) -> Writer {
        Writer(self.0.clone())
    }
}

struct ArrivalCheckedBody {
    buffer: Buffer,
    sent: bool,
}

impl futures_core::Stream for ArrivalCheckedBody {
    type Item = Result<axum::body::Bytes, std::io::Error>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        if self.sent {
            return std::task::Poll::Ready(None);
        }
        let logs = String::from_utf8(self.buffer.0.lock().unwrap().clone()).unwrap();
        assert!(
            logs.contains("protocol request arrival") && logs.contains("body-read-1"),
            "arrival must precede body polling: {logs}"
        );
        self.sent = true;
        std::task::Poll::Ready(Some(Ok(axum::body::Bytes::from_static(b"{}"))))
    }
}

#[tokio::test]
async fn protocol_arrival_is_logged_before_body_is_read() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let buffer = Buffer::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buffer.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    let request = Request::builder()
        .method("POST")
        .uri("/Sessions/Playing")
        .header("authorization", "Bearer admin-token")
        .header("content-type", "application/json")
        .header("x-request-id", "body-read-1")
        .body(Body::from_stream(ArrivalCheckedBody {
            buffer,
            sent: false,
        }))
        .unwrap();
    let _response = app.oneshot(request).await.unwrap();
    drop(guard);
}

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    id: &str,
    token: Option<&str>,
    body: String,
) -> StatusCode {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", id)
        .header("content-type", "application/json");
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    app.clone()
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap()
        .status()
}

fn assert_observed(logs: &str, id: &str, method: &str, route: &str, status: u16) {
    let lines: Vec<_> = logs.lines().filter(|line| line.contains(id)).collect();
    assert_eq!(
        lines.len(),
        2,
        "expected arrival and completion for {id}: {logs}"
    );
    assert!(lines[0].contains("protocol request arrival"), "{logs}");
    assert!(lines[1].contains("protocol request complete"), "{logs}");
    for line in lines {
        assert!(line.contains(method) && line.contains(route), "{line}");
    }
    assert!(
        logs.lines().any(|line| line.contains(id)
            && line.contains(&format!("status={status}"))
            && line.contains("headers_latency_ms=")),
        "{logs}"
    );
}

#[tokio::test]
async fn protocol_requests_are_observed_without_sensitive_data() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, item_id) = app(tmp.path());
    let session = play_session_for(&app, &item_id, None).await;
    let buffer = Buffer::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buffer.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);

    assert_eq!(
        send(
            &app,
            "GET",
            "/System/Info/Public?api_key=query-secret",
            "direct-1",
            None,
            String::new()
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        send(
            &app,
            "GET",
            "/emby/System/Info/Public",
            "emby-1",
            None,
            String::new()
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        send(
            &app,
            "POST",
            "/Sessions/Playing",
            "denied-1",
            None,
            "body-secret".into()
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    let body = json!({"ItemId": item_id, "PlaySessionId": session, "PositionTicks": 0, "Secret": "body-secret"}).to_string();
    assert_eq!(
        send(
            &app,
            "POST",
            "/Sessions/Playing?unused=query-secret",
            "playing-1",
            Some("admin-token"),
            body
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send(
            &app,
            "POST",
            &format!("/Items/{item_id}/PlaybackInfo"),
            "item-1",
            Some("admin-token"),
            "{}".into()
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        send(
            &app,
            "GET",
            "/unknown-private-id?token=query-secret",
            "missing-1",
            None,
            String::new()
        )
        .await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(
            &app,
            "GET",
            "/api/v1/health",
            "health-1",
            None,
            String::new()
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        send(
            &app,
            "GET",
            "/api/v1/sites",
            "api-1",
            Some("admin-token"),
            String::new()
        )
        .await,
        StatusCode::OK
    );
    drop(guard);

    let logs = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    assert_safe_logs(&logs, &item_id, &session);
}

fn assert_safe_logs(logs: &str, item_id: &str, session: &str) {
    assert_observed(logs, "direct-1", "GET", "/System/Info/Public", 200);
    assert_observed(logs, "emby-1", "GET", "/emby/System/Info/Public", 200);
    assert_observed(logs, "denied-1", "POST", "/Sessions/Playing", 401);
    assert_observed(logs, "playing-1", "POST", "/Sessions/Playing", 204);
    assert_observed(logs, "item-1", "POST", "/Items/{id}/PlaybackInfo", 200);
    assert_observed(logs, "missing-1", "GET", "unmatched", 404);
    assert!(logs.lines().any(|line| line.contains("direct-1")
        && line.contains("DEBUG")
        && line.contains("protocol request complete")));
    assert!(logs.lines().any(|line| line.contains("denied-1")
        && line.contains("WARN")
        && line.contains("protocol request complete")));
    for secret in [
        "query-secret",
        "body-secret",
        "admin-token",
        "unknown-private-id",
        item_id,
        session,
    ] {
        assert!(!logs.contains(secret), "sensitive value leaked: {logs}");
    }
    assert!(
        !logs.contains("health-1"),
        "API health must retain its logging exclusion: {logs}"
    );
    let api_lines: Vec<_> = logs.lines().filter(|line| line.contains("api-1")).collect();
    assert_eq!(
        api_lines.len(),
        1,
        "API requests must retain only their existing log: {logs}"
    );
    assert!(!api_lines[0].contains("protocol request"), "{logs}");
}
