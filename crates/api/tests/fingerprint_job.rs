use api::fingerprint_job::analyze_fingerprints_with_outro;
use domain::{Confidence, LedgerId, LedgerRow, MediaId, QualitySource};

fn row(media_id: MediaId, episode: u32) -> LedgerRow {
    LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: format!("episode-{episode}.mkv"),
        season: Some(1),
        episode: Some(episode),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
    }
}

fn word(hash: u32, payload: u32) -> u32 {
    (hash << 20) | (payload & 0x000f_ffff)
}

fn split_three_minute_match() -> (Vec<u32>, Vec<u32>) {
    let mut first = Vec::new();
    let mut second = Vec::new();
    for index in 0..1454 {
        let hash = 500 + index as u32;
        let payload = (index as u32 * 7919 + 31) & 0x000f_ffff;
        first.push(word(hash, payload));
        second.push(word(
            hash,
            if index < 840 {
                payload
            } else {
                payload ^ 0b1111
            },
        ));
    }
    (first, second)
}

#[test]
fn cached_outro_fingerprints_produce_full_credit_markers() {
    let media_id = MediaId::new();
    let (first, second) = split_three_minute_match();
    let markers = analyze_fingerprints_with_outro(
        media_id,
        1,
        Vec::new(),
        vec![
            (row(media_id, 1), first, 600_000),
            (row(media_id, 2), second, 900_000),
        ],
    );

    assert_eq!(markers.len(), 2);
    for (episode, expected_start) in [(1, 600_000), (2, 900_000)] {
        let marker = markers
            .iter()
            .find(|marker| marker.episode == episode)
            .unwrap();
        assert_eq!(marker.outro_start_ms, Some(expected_start));
        assert!(
            (175_000..=185_000)
                .contains(&(marker.outro_end_ms.unwrap() - marker.outro_start_ms.unwrap()))
        );
        assert_eq!(marker.intro_start_ms, None);
    }
}
