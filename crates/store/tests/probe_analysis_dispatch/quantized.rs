use super::*;

#[test]
fn analysis_context_identity_changes_when_only_origin_precision_changes() {
    let f = Fixture::new();
    let request = f.captured(1);
    let mut cache = f
        .store
        .get_fingerprint_cache(&request.dispatch.ledger_id)
        .unwrap()
        .unwrap();
    cache.intro.metrics.quantized_mapping = Some(marker::QuantizedPcmMapping {
        origin_min_us: -500,
        origin_max_us: 500,
        native_sample_rate: 44_100,
        source_tick_us: 1000,
        packet_count: 7800,
        block_anchors: 975,
        decoded_native_samples: 7_987_200,
        codec_native_samples: 7_987_200,
        encoded_payload_sha256: "a".repeat(64),
        proof_version: 1,
    });
    cache.intro.metrics.sample_count = Some(2_880_000);
    cache.intro.metrics.pcm_bytes = 5_760_000;
    f.store
        .put_fingerprint_cache(&request.dispatch.ledger_id, &cache)
        .unwrap();
    let old = f.store.prepare_probe_analysis(&request).unwrap();
    cache
        .intro
        .metrics
        .quantized_mapping
        .as_mut()
        .unwrap()
        .origin_min_us = -600;
    f.store
        .put_fingerprint_cache(&request.dispatch.ledger_id, &cache)
        .unwrap();
    let current = f.store.prepare_probe_analysis(&request).unwrap();
    assert_ne!(
        old.context().context_key,
        current.context().context_key,
        "origin-range changes must dirty derived Analysis despite identical words and capture window"
    );
}

#[test]
fn analysis_does_not_treat_old_header_schema_as_current_source_facts() {
    let f = Fixture::new();
    let request = f.captured(1);
    f.store
        .put_file_meta_versioned(
            &request.dispatch.ledger_id,
            &library::Tracks {
                video: Some(Default::default()),
                audio: vec![library::AudioTrack {
                    stream_index: Some(0),
                    ..Default::default()
                }],
                ..Default::default()
            },
            Some(&request.target.file.version.source_version),
            Some(1_800_000),
        )
        .unwrap();
    f.store
        .with_writer(store::Db::Library, |db| {
            db.execute(
                "UPDATE file_details SET tracks_version=?2 WHERE file_id=?1",
                rusqlite::params![
                    request.dispatch.ledger_id,
                    store::library_write::probe::CURRENT_TRACKS_VERSION - 1
                ],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(
        f.store
            .get_media_info_cache_version(&request.dispatch.ledger_id)
            .unwrap()
            .is_none()
    );
    let input = f
        .store
        .read_probe_analysis(&request)
        .unwrap()
        .prepare()
        .unwrap();
    assert!(
        input.duration_ms.is_none(),
        "Analysis must share current metadata version admission"
    );
    assert!(input.audio.is_empty());
}
