//! Interactive reads and durable login must remain usable during a Library write batch.
#[path = "reads_during_scan/app_admission.rs"]
mod app_admission;
#[path = "reads_during_scan/app_heartbeat.rs"]
mod app_heartbeat;
#[path = "reads_during_scan/app_writer.rs"]
mod app_writer;
#[path = "reads_during_scan/doubles.rs"]
mod doubles;
#[path = "reads_during_scan/fixture.rs"]
mod fixture;
#[path = "reads_during_scan/requests.rs"]
mod requests;

use axum::http::StatusCode;
use fixture::{Fixture, MEMBER_TOKEN};
use requests::{HeldWriter, exchange, get, login};
use std::time::{Duration, Instant};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interactive_reads_succeed_while_writer_is_held() {
    let fixture = Fixture::new();
    let app = api::router(fixture.state.clone());
    let held = HeldWriter::acquire(&fixture.state);
    let started = Instant::now();
    // Spawn every request before awaiting any result. The shared start also counts
    // scheduling delays; a task starting after the batch cannot get a fresh budget.
    let login = tokio::spawn(exchange(app.clone(), login(), started));
    let views = tokio::spawn(exchange(
        app.clone(),
        get(&format!("/Users/{}/Views", fixture.user), MEMBER_TOKEN),
        started,
    ));
    let item = tokio::spawn(exchange(
        app.clone(),
        get(
            &format!("/Users/{}/Items/{}", fixture.user, fixture.item),
            MEMBER_TOKEN,
        ),
        started,
    ));
    let jobs = tokio::spawn(exchange(
        app.clone(),
        get("/api/v1/jobs", MEMBER_TOKEN),
        started,
    ));
    let subscriptions = tokio::spawn(exchange(
        app.clone(),
        get("/api/v1/subscriptions", MEMBER_TOKEN),
        started,
    ));
    let (login, views, item, jobs, subscriptions) =
        tokio::join!(login, views, item, jobs, subscriptions);
    let results = [
        ("login", login.unwrap()),
        ("views", views.unwrap()),
        ("item", item.unwrap()),
        ("jobs", jobs.unwrap()),
        ("subscriptions", subscriptions.unwrap()),
    ];
    let all_timely = results
        .iter()
        .all(|(_, response)| response.elapsed < Duration::from_secs(1));
    // A real second HTTP request must see the committed token while the original
    // outer Store is still held. No token value or response body enters diagnostics.
    let authenticated = if all_timely && results[0].1.status == StatusCode::OK && held.is_held() {
        let token = results[0].1.body["data"]["token"].as_str().unwrap();
        assert!(token != MEMBER_TOKEN, "login must issue a new session");
        Some(exchange(app, get("/api/v1/auth/me", token), Instant::now()).await)
    } else {
        None
    };
    held.finish();
    for (name, response) in &results {
        assert!(
            response.elapsed < Duration::from_secs(1),
            "{name} waited for the three-second writer batch: {:?} ({})",
            response.elapsed,
            response.status
        );
        assert_eq!(response.status, StatusCode::OK, "{name}");
    }
    let authenticated = authenticated.expect("login token must be usable during the writer batch");
    assert_eq!(authenticated.status, StatusCode::OK);
    assert!(authenticated.elapsed < Duration::from_secs(1));
    assert_eq!(authenticated.body["data"]["id"], fixture.user.to_string());
    assert!(
        results[1].1.body["Items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| {
                row["Id"] == fixture.library.to_string() && row["Name"] == "Interactive Library"
            })
    );
    assert_eq!(results[2].1.body["Id"], fixture.item);
    assert_eq!(results[2].1.body["Overview"], "adopted interactive plot");
    assert!(
        results[3].1.body["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| { row["id"] == fixture.job.to_string() })
    );
    let subscriptions = results[4].1.body["data"].as_array().unwrap();
    assert_eq!(subscriptions.len(), 1);
    assert_eq!(subscriptions[0]["id"], fixture.subscribe.to_string());
    assert_eq!(subscriptions[0]["media"]["title"], "Interactive series");
}
