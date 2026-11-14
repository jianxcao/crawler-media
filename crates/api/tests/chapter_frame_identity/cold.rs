//! The cold chapter resolver creates durable work without a running dispatcher.
use super::*;

struct Chapters;
impl api::marker_resolver::ChapterProbe for Chapters {
    fn probe(&self, _: &Path) -> Option<Vec<marker::Chapter>> {
        Some(vec![
            marker::Chapter {
                start_ms: 0,
                end_ms: 10_000,
                title: Some("Opening".into()),
            },
            marker::Chapter {
                start_ms: 10_000,
                end_ms: 20_000,
                title: Some("Content".into()),
            },
        ])
    }
}

#[test]
fn cold_resolver_persists_its_visible_timeline_and_frame_obligation() {
    let mut f = Fixture::new(&[UnitKey::new(1, 1)]);
    f.store
        .clear_cached_chapters(&f.files[0].file_id.to_string())
        .unwrap();
    f.state = f.state.with_chapter_probe(Arc::new(Chapters));
    f.app = api::router(f.state.clone());
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let items = f.chapters(UnitKey::new(1, 1)).await;
        let current = f
            .store
            .current_chapter_frame_task(f.files[0].file_id)
            .unwrap()
            .expect("cold resolution must commit a durable frame obligation");
        let mut visible = items
            .iter()
            .map(|item| {
                (
                    item["start_ms"].as_i64().unwrap(),
                    item["end_ms"].as_i64().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        visible.sort_unstable();
        visible.dedup();
        assert_eq!(current.task.targets, visible);
        assert_eq!(current.status.state, store::ChapterFrameState::Queued);
        let old_id = current.task.id;
        assert_eq!(f.chapters(UnitKey::new(1, 1)).await, items);
        assert_eq!(
            f.store
                .current_chapter_frame_task(f.files[0].file_id)
                .unwrap()
                .unwrap()
                .task
                .id,
            old_id
        );
        f.publish(0);
        for item in f.chapters(UnitKey::new(1, 1)).await {
            assert_eq!(
                f.get(item["image_url"].as_str().unwrap()).await.status(),
                StatusCode::OK
            );
        }
    });
}
