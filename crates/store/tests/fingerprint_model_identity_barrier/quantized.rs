use super::*;

pub(super) fn metrics() -> marker::CaptureMetrics {
    metrics_at_rate(44_100)
}

fn metrics_at_rate(rate: u32) -> marker::CaptureMetrics {
    let source = marker::CaptureSourceFacts {
        audio_stream_index: 0,
        format_start_time_us: Some(0),
        stream_start_pts: Some(0),
        stream_time_base: Some("1/1000".into()),
        codec_name: Some("aac".into()),
        native_sample_rate: Some(rate),
        audio_channels: Some(2),
        provenance: "fixture_header_v2".into(),
    };
    let packets = if rate == 48_000 { 8440 } else { 7800 };
    marker::CaptureMetrics {
        time_mapping_version: 4,
        audio_stream_index: Some(0),
        source_facts: Some(source),
        pcm_timeline: marker::PcmTimeline::Contiguous,
        sample_count: Some(2_880_000),
        pcm_bytes: 5_760_000,
        quantized_mapping: Some(marker::QuantizedPcmMapping {
            origin_min_us: -500,
            origin_max_us: 500,
            native_sample_rate: rate,
            source_tick_us: 1000,
            packet_count: packets,
            block_anchors: 975,
            decoded_native_samples: packets * 1024,
            codec_native_samples: packets * 1024,
            encoded_payload_sha256: "a".repeat(64),
            proof_version: 1,
        }),
        ..Default::default()
    }
}

#[test]
fn model_accepts_ranged_capture_and_binds_origin_precision_in_identity() {
    let mut f = Fixture::new();
    for sample in &mut f.samples {
        let metrics = metrics();
        sample.capture_profile_key = marker::capture_profile::capture_profile_key(
            &marker::capture_profile::profile_for_source(metrics.source_facts.as_ref()),
        );
        sample.metrics_json = serde_json::to_string(&metrics).unwrap();
    }
    let first = model_bundle::bundle(f.loser, &f.samples);
    let mut altered: marker::CaptureMetrics =
        serde_json::from_str(&f.samples[0].metrics_json).unwrap();
    altered.quantized_mapping.as_mut().unwrap().origin_min_us = -600;
    f.samples[0].metrics_json = serde_json::to_string(&altered).unwrap();
    let changed = model_bundle::bundle(f.loser, &f.samples);
    assert_ne!(first.model.membership_key, changed.model.membership_key);
    assert_ne!(first.model.model_id, changed.model.model_id);
}

#[test]
fn model_accepts_aac48_and_rejects_source_proof_rate_mismatch() {
    let mut f = Fixture::new();
    for sample in &mut f.samples {
        let metrics = metrics_at_rate(48_000);
        sample.capture_profile_key = marker::capture_profile::capture_profile_key(
            &marker::capture_profile::profile_for_source(metrics.source_facts.as_ref()),
        );
        sample.metrics_json = serde_json::to_string(&metrics).unwrap();
    }
    let valid = model_bundle::bundle(f.loser, &f.samples);
    let template: marker::TemplateModel = serde_json::from_str(&valid.model.model_json).unwrap();
    for alter_source in [false, true] {
        let mut samples = f.samples.clone();
        let mut metrics = metrics_at_rate(48_000);
        if alter_source {
            metrics.source_facts.as_mut().unwrap().native_sample_rate = Some(44_100);
        } else {
            metrics
                .quantized_mapping
                .as_mut()
                .unwrap()
                .native_sample_rate = 44_100;
        }
        samples[0].metrics_json = serde_json::to_string(&metrics).unwrap();
        let result = store::StoredFingerprintModelBundle::new(
            f.loser,
            1,
            "policy".into(),
            template.clone(),
            samples,
            1000,
        );
        assert!(
            result.is_err(),
            "model must reject rate mismatch (source={alter_source})"
        );
    }
}
