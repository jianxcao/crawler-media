//! Public NextUp/Resume regression: a single physical file covers many units.

#[path = "jellyfin_shared_coverage/allocations.rs"]
mod allocations;
#[path = "jellyfin_shared_coverage/behavior.rs"]
mod behavior;
#[path = "jellyfin_shared_coverage/fixture.rs"]
mod fixture;

use allocations::{MeasuredAllocator, Measurement};
use fixture::Fixture;
use media_server::provider::MediaServerProvider;

#[global_allocator]
static ALLOCATOR: MeasuredAllocator = MeasuredAllocator;

#[derive(Clone, Copy, Debug)]
enum Page {
    NextUp,
    Resume,
}

fn measured_page(fixture: &Fixture, runtime: &tokio::runtime::Runtime, page: Page) -> usize {
    let provider = fixture.provider();
    let measurement = Measurement::start();
    let response = runtime
        .block_on(async {
            match page {
                Page::NextUp => provider.next_up_items(fixture.user, None, 0, 1).await,
                Page::Resume => {
                    provider
                        .resumable_items_page(fixture.user, None, 0, 1)
                        .await
                }
            }
        })
        .unwrap();
    let bytes = measurement.finish();
    assert_eq!(response.total, 1);
    assert_eq!(response.items.len(), 1);
    let item = &response.items[0];
    assert_eq!(item.media.id, fixture.media);
    assert_eq!(item.file.file_id, fixture.source.file_id);
    let episode = match page {
        Page::NextUp => 1,
        Page::Resume => 2,
    };
    assert_eq!(item.unit, Some(domain::UnitKey::new(1, episode)));
    assert!(!item.played);
    if matches!(page, Page::Resume) {
        assert_eq!(item.ticks, 200_000_000);
        assert!(item.metadata.is_favorite);
    }
    bytes
}

#[test]
fn nextup_and_resume_share_multiunit_coverage() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut samples = Vec::new();
    for episodes in [100, 1000, 5000] {
        let fixture = Fixture::new(episodes);
        // Warm the runtime blocking pool before enabling the allocator observer.
        measured_page(&fixture, &runtime, Page::NextUp);
        let nextup = measured_page(&fixture, &runtime, Page::NextUp);
        let resume = measured_page(&fixture, &runtime, Page::Resume);
        eprintln!("units={episodes} nextup_bytes={nextup} resume_bytes={resume}");
        samples.push((nextup, resume));
    }
    behavior::marks_and_versions(&runtime);
    behavior::acl_and_user_state(&runtime);
    for (index, page) in [Page::NextUp, Page::Resume].into_iter().enumerate() {
        let values: Vec<_> = samples
            .iter()
            .map(|pair| if index == 0 { pair.0 } else { pair.1 })
            .collect();
        assert!(
            values[1] <= 15 * values[0],
            "{page:?} allocations grew superlinearly: {values:?}"
        );
        assert!(
            values[2] <= 8 * values[1],
            "{page:?} allocations grew superlinearly: {values:?}"
        );
    }
}
