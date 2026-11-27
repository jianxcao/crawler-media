use std::sync::Arc;

use domain::{SiteId, Torrent};
use hooks::{HookPipeline, NativeCandidate, PluginInvoker, Registration};
use plugin_api::{FilterOutcome, SearchAfter};

struct DropFirst;
impl PluginInvoker for DropFirst {
    fn search_after(&self, mut input: SearchAfter) -> FilterOutcome {
        input.candidates.remove(0);
        FilterOutcome {
            candidates: input.candidates,
            site_failures: input.site_failures,
            failure: None,
        }
    }
}

fn torrent(site_id: SiteId, title: &str, enclosure: &str) -> Torrent {
    Torrent {
        site_id,
        title: title.into(),
        enclosure: enclosure.into(),
        size_bytes: None,
        seeders: None,
        free: true,
        hr: None,
        imdb_id: None,
        id: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
        free_2x: false,
        free_until: None,
    }
}

#[test]
fn survivors_are_the_original_native_torrents() {
    let pipeline = HookPipeline::new();
    pipeline.register(Registration::new(0, "p", "c", Arc::new(DropFirst)));
    let site = SiteId::new();
    let original = torrent(site, "B", "https://example.test/download?passkey=secret");
    let input = vec![
        NativeCandidate::new("a", "provider", 3, 9, torrent(site, "A", "private-a")),
        NativeCandidate::new("b", "provider", 3, 9, original.clone()),
    ];
    let output = pipeline.filter_native(input, vec![]).unwrap();
    assert_eq!(output.candidates.len(), 1);
    assert_eq!(output.candidates[0].torrent, original);
    assert_eq!(output.candidates[0].handle, "b");
}

#[test]
fn projection_preserves_known_timestamp_and_omits_private_urls() {
    struct Inspect;
    impl PluginInvoker for Inspect {
        fn search_after(&self, input: SearchAfter) -> FilterOutcome {
            assert_eq!(input.candidates[0].published_at, Some(1_704_067_200_000));
            assert!(!format!("{:?}", input.candidates).contains("private-secret"));
            FilterOutcome {
                candidates: input.candidates,
                site_failures: input.site_failures,
                failure: None,
            }
        }
    }
    let pipeline = HookPipeline::new();
    pipeline.register(Registration::new(0, "p", "c", Arc::new(Inspect)));
    let mut native = torrent(
        SiteId::new(),
        "A",
        "https://example.test/?passkey=private-secret",
    );
    native.upload_time = Some("2024-01-01T00:00:00Z".into());
    pipeline
        .filter_native(
            vec![NativeCandidate::new("a", "provider", 1, 2, native)],
            vec![],
        )
        .unwrap();
}

#[test]
fn malformed_filter_output_keeps_prior_native_input() {
    struct Forge;
    impl PluginInvoker for Forge {
        fn search_after(&self, mut input: SearchAfter) -> FilterOutcome {
            input.candidates[0].title = "forged".into();
            FilterOutcome {
                candidates: input.candidates,
                site_failures: input.site_failures,
                failure: None,
            }
        }
    }
    let pipeline = HookPipeline::new();
    pipeline.register(Registration::new(0, "p", "c", Arc::new(Forge)));
    let original = torrent(SiteId::new(), "A", "private");
    let result = pipeline
        .filter_native(
            vec![NativeCandidate::new(
                "a",
                "provider",
                1,
                2,
                original.clone(),
            )],
            vec![],
        )
        .unwrap();
    assert_eq!(result.candidates[0].torrent, original);
    assert!(matches!(
        &result.diagnostics[0].failure,
        plugin_api::Failure::InvalidPatch(_)
    ));
}

#[test]
fn candidate_timestamp_parser_handles_rfc3339_rfc2822_sql_utc_and_epoch_units() {
    use hooks::payload::parse_candidate_published_at_ms;

    let expected = 1_704_067_200_000u64;
    assert_eq!(
        parse_candidate_published_at_ms("2024-01-01T00:00:00Z"),
        Some(expected)
    );
    assert_eq!(
        parse_candidate_published_at_ms("2024-01-01T08:00:00+08:00"),
        Some(expected)
    );
    assert_eq!(
        parse_candidate_published_at_ms("Mon, 01 Jan 2024 00:00:00 +0000"),
        Some(expected)
    );
    assert_eq!(
        parse_candidate_published_at_ms("2024-01-01 00:00:00"),
        Some(expected)
    );
    assert_eq!(
        parse_candidate_published_at_ms("2024-01-01 00:00"),
        Some(expected)
    );
    assert_eq!(
        parse_candidate_published_at_ms("1704067200"),
        Some(expected)
    );
    assert_eq!(
        parse_candidate_published_at_ms("1704067200456"),
        Some(1_704_067_200_456)
    );
    assert_eq!(parse_candidate_published_at_ms("not-a-date"), None);
    assert_eq!(parse_candidate_published_at_ms(""), None);

    struct InspectFormats;
    impl PluginInvoker for InspectFormats {
        fn search_after(&self, input: SearchAfter) -> FilterOutcome {
            for cand in &input.candidates {
                assert_eq!(
                    cand.published_at,
                    Some(1_704_067_200_000),
                    "handle={}",
                    cand.handle
                );
            }
            FilterOutcome {
                candidates: input.candidates,
                site_failures: input.site_failures,
                failure: None,
            }
        }
    }
    let pipeline = HookPipeline::new();
    pipeline.register(Registration::new(0, "p", "c", Arc::new(InspectFormats)));
    let site = SiteId::new();
    let inputs: Vec<_> = [
        ("rfc2822", "Mon, 01 Jan 2024 00:00:00 +0000"),
        ("sql_sec", "2024-01-01 00:00:00"),
        ("sql_min", "2024-01-01 00:00"),
        ("epoch_s", "1704067200"),
        ("epoch_ms", "1704067200000"),
    ]
    .into_iter()
    .map(|(h, ts)| {
        let mut t = torrent(site, "A", "https://example.test/dl");
        t.upload_time = Some(ts.into());
        NativeCandidate::new(h, "provider", 1, 1, t)
    })
    .collect();
    pipeline.filter_native(inputs, vec![]).unwrap();
}
