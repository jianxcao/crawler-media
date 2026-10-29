//! Public HTTP delivery must return a completed image or a final bounded error.
#[path = "media_poster_delivery/admission.rs"]
mod admission;
#[path = "support/cache_read_fifo.rs"]
mod cache_read_fifo;
#[path = "media_poster_delivery/support.rs"]
mod support;
use api::media_posters::{MediaPosterSource, PosterError};
use axum::body::to_bytes;
use axum::http::{Request, StatusCode};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use support::*;
use tower::ServiceExt;

#[test]
fn earlier_poster_observer_keeps_original_deadline_when_joining_later_fill() {
    let source = FiniteSource::new(true, Duration::ZERO, Outcome::Image);
    let fixture = Fixture::new(source.clone(), Duration::from_millis(600), 1);
    let (cache_read, entered) = cache_read_fifo::CacheReadPause::new(&fixture.cache_path(0));
    runtime().block_on(async {
        let _release = source.release_on_drop();
        let started = Instant::now();
        let earlier = tokio::spawn(get(fixture.app(), fixture.ids[0]));
        entered.await.unwrap();
        tokio::time::sleep(Duration::from_millis(250)).await;
        let later = tokio::spawn(get(fixture.app(), fixture.ids[0]));
        until("later observer starts the real shared source", || {
            source.active.load(Ordering::SeqCst) == 1
        })
        .await;
        cache_read.finish();
        until("both HTTP observers joined the shared fill", || {
            fixture.service.inflight_waiters(fixture.ids[0]) == 2
        })
        .await;
        let earlier_result = tokio::time::timeout_at(
            tokio::time::Instant::from_std(started + Duration::from_millis(725)),
            earlier,
        )
        .await;
        let active = source.active.load(Ordering::SeqCst);
        let later_pending = !later.is_finished();
        source.release();
        let later_response = later.await.unwrap();
        fixture.drained().await;
        let earlier_response = earlier_result
            .expect("joining a later flight must not extend the earlier HTTP deadline")
            .unwrap();
        assert_eq!(earlier_response.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(
            active, 1,
            "an observer deadline cannot release a real worker"
        );
        assert!(
            later_pending,
            "the later observer must retain its own budget"
        );
        image(later_response).await;
    });
}

#[test]
fn media_poster_cold_fill_returns_image_without_warming_503() {
    let fixture = Fixture::new(
        FiniteSource::new(false, Duration::from_millis(250), Outcome::Image),
        Duration::from_secs(1),
        1,
    );
    runtime().block_on(async {
        let response = get(fixture.app(), fixture.ids[0]).await;
        let etag = response.headers().get("etag").cloned();
        image(response).await;
        let etag = etag.expect("completed cold images need conditional validators");
        image(get(fixture.app(), fixture.ids[0]).await).await;
        let response = fixture
            .app()
            .oneshot(
                Request::get(format!("/api/v1/media/{}/poster", fixture.ids[0]))
                    .header("if-none-match", etag)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert!(
            to_bytes(response.into_body(), 1024)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            get(fixture.app(), domain::MediaId::new()).await.status(),
            StatusCode::NOT_FOUND
        );
        fixture.drained().await;
    });
}

#[test]
fn media_poster_concurrent_cold_requests_share_fill() {
    let source = FiniteSource::new(true, Duration::ZERO, Outcome::Image);
    let fixture = Fixture::new(source.clone(), Duration::from_secs(1), 1);
    runtime().block_on(async {
        let _release = source.release_on_drop();
        let handles: Vec<_> = (0..8)
            .map(|_| tokio::spawn(get(fixture.app(), fixture.ids[0])))
            .collect();
        until("one real upstream worker starts", || {
            source.active.load(Ordering::SeqCst) == 1
        })
        .await;
        tokio::time::sleep(Duration::from_millis(25)).await;
        let pending = handles.iter().all(|task| !task.is_finished());
        let live = fixture.service.resource_counts();
        source.release();
        let mut responses = Vec::new();
        for task in handles {
            responses.push(task.await.unwrap());
        }
        fixture.drained().await;
        assert!(
            pending,
            "cold observers must await the shared terminal result"
        );
        assert_eq!(live.registry_entries, 1);
        assert_eq!(source.peak.load(Ordering::SeqCst), 1);
        for response in responses {
            image(response).await;
        }
    });
}

#[test]
fn media_poster_stalled_source_releases_waiters_at_deadline() {
    let source = FiniteSource::new(false, Duration::from_millis(180), Outcome::Timeout);
    let fixture = Fixture::new(source.clone(), Duration::from_millis(70), 2);
    runtime().block_on(async {
        let started = Instant::now();
        let response = get(fixture.app(), fixture.ids[0]).await;
        let status = response.status();
        let cache_control = response.headers().get("cache-control").cloned();
        let elapsed = started.elapsed();
        fixture.drained().await;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(cache_control.unwrap(), "no-store");
        assert!(elapsed < Duration::from_millis(400));
        assert_eq!(source.active.load(Ordering::SeqCst), 0);
        source.succeed();
        image(get(fixture.app(), fixture.ids[1]).await).await;
        fixture.drained().await;
    });
}

#[test]
fn media_poster_preserves_actual_upstream_failure() {
    for outcome in [Outcome::Upstream, Outcome::Invalid] {
        let fixture = Fixture::new(
            FiniteSource::new(false, Duration::ZERO, outcome),
            Duration::from_secs(1),
            1,
        );
        runtime().block_on(async {
            let response = get(fixture.app(), fixture.ids[0]).await;
            let status = response.status();
            let headers = response.headers().clone();
            fixture.drained().await;
            assert_eq!(status, StatusCode::BAD_GATEWAY);
            assert_eq!(headers["cache-control"], "no-store");
            assert!(!headers.contains_key("retry-after"));
        });
    }
}

#[test]
fn cancelled_media_poster_observer_does_not_cancel_shared_fill() {
    let source = FiniteSource::new(true, Duration::ZERO, Outcome::Image);
    let fixture = Fixture::new(source.clone(), Duration::from_secs(1), 1);
    runtime().block_on(async {
        let _release = source.release_on_drop();
        let leader = tokio::spawn(get(fixture.app(), fixture.ids[0]));
        until("upstream started", || {
            source.active.load(Ordering::SeqCst) == 1
        })
        .await;
        let follower = tokio::spawn(get(fixture.app(), fixture.ids[0]));
        tokio::time::sleep(Duration::from_millis(25)).await;
        leader.abort();
        let _ = leader.await;
        let active_after_cancel = source.active.load(Ordering::SeqCst);
        source.release();
        let response = follower.await.unwrap();
        fixture.drained().await;
        assert_eq!(
            active_after_cancel, 1,
            "real worker retains its permit after one observer leaves"
        );
        assert_eq!(source.peak.load(Ordering::SeqCst), 1);
        image(response).await;
    });
}

#[test]
fn queued_media_poster_deadline_prevents_a_late_fill() {
    let source = FiniteSource::new(true, Duration::ZERO, Outcome::Image);
    let fixture = Fixture::new(source.clone(), Duration::from_millis(80), 5);
    runtime().block_on(async {
        let _release = source.release_on_drop();
        let workers: Vec<_> = fixture.ids[..4]
            .iter()
            .map(|id| {
                let service = fixture.service.clone();
                let id = *id;
                tokio::spawn(async move { service.get(id).await })
            })
            .collect();
        until("four real workers occupy upstream capacity", || {
            source.active.load(Ordering::SeqCst) == 4
        })
        .await;
        let response = get(fixture.app(), fixture.ids[4]).await;
        let status = response.status();
        let still_active = source.active.load(Ordering::SeqCst);
        source.release();
        for worker in workers {
            let outcome = worker.await.unwrap();
            assert!(outcome.is_ok() || matches!(outcome, Err(PosterError::Timeout)));
        }
        fixture.drained().await;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(
            still_active, 4,
            "observer timeout must not release an actual worker's slot"
        );
        assert_eq!(source.peak.load(Ordering::SeqCst), 4);
        assert!(
            fixture
                .service
                .cached(fixture.ids[4])
                .await
                .unwrap()
                .is_none(),
            "an expired queued request must never perform a late source fetch/cache publication"
        );
    });
}
