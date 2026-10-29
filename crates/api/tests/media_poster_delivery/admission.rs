use super::*;

#[test]
fn direct_media_poster_get_obeys_the_same_unique_admission_window() {
    let source = FiniteSource::new(true, Duration::ZERO, Outcome::Image);
    let fixture = Fixture::new(source.clone(), Duration::from_millis(500), 34);
    runtime().block_on(async {
        let _release = source.release_on_drop();
        let leaders: Vec<_> = fixture.ids[..32]
            .iter()
            .map(|id| {
                let service = fixture.service.clone();
                let id = *id;
                tokio::spawn(async move { service.get(id).await })
            })
            .collect();
        until("the shared unique admission window is full", || {
            fixture.service.resource_counts().registry_entries == 32
        })
        .await;
        until("all upstream slots remain held by real workers", || {
            source.active.load(Ordering::SeqCst) == 4
        })
        .await;
        let service = fixture.service.clone();
        let shared_id = fixture.ids[0];
        let follower = tokio::spawn(async move { service.get(shared_id).await });
        until("an existing key can still join at capacity", || {
            fixture.service.inflight_waiters(shared_id) >= 2 || follower.is_finished()
        })
        .await;
        let rejected = tokio::time::timeout(
            Duration::from_millis(100),
            fixture.service.get(fixture.ids[32]),
        )
        .await;
        let admitted = fixture.service.resource_counts().registry_entries;
        let actual_peak = source.peak.load(Ordering::SeqCst);
        source.release();
        for leader in leaders {
            let result = leader.await.unwrap();
            assert!(result.is_ok() || matches!(result, Err(PosterError::Timeout)));
        }
        let joined = follower.await.unwrap();
        fixture.drained().await;
        assert!(
            matches!(rejected, Ok(Err(PosterError::Busy))),
            "a new unique key must fail promptly while existing fills retain capacity: {rejected:?}"
        );
        assert!(admitted <= 32, "get bypassed the shared window: {admitted}");
        assert!(actual_peak <= 4);
        assert_eq!(joined.unwrap().bytes.as_ref(), IMAGE);
        image(get(fixture.app(), fixture.ids[33]).await).await;
        fixture.drained().await;
    });
}
