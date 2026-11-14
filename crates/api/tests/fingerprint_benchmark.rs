use api::fingerprint_job::{
    CapturePolicy,
    benchmark::{BenchmarkOptions, BenchmarkSession},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::Path;
#[path = "fingerprint_benchmark/lifecycle.rs"]
mod lifecycle;

fn options(root: &Path, episodes: &[i32]) -> BenchmarkOptions {
    let mut cases = Vec::new();
    let mut sources = Vec::new();
    for &episode in episodes {
        let source = root.join(format!("episode-{episode}.mp4"));
        if !source.exists() {
            std::fs::write(&source, format!("local-engineering-source-{episode}")).unwrap();
        }
        let hash = format!(
            "sha256:{:x}",
            Sha256::digest(std::fs::read(&source).unwrap())
        );
        for kind in ["intro", "outro"] {
            let case = format!("opaque-{episode}-{kind}");
            cases.push(json!({"case_id":case,"media_group":"m1","split":"development","source_hash":hash,"duration_ms":400000,"kind":kind}));
            sources.push(json!({"case_id":case,"path":source,"audio_stream_index":1,"unit":{"season":1,"episode":episode}}));
        }
    }
    let manifest = root.join("manifest.json");
    let mappings = root.join("sources.json");
    std::fs::write(
        &manifest,
        serde_json::to_vec(
            &json!({"corpus_version":"engineering-v1","evidence_kind":"synthetic","cases":cases}),
        )
        .unwrap(),
    )
    .unwrap();
    std::fs::write(&mappings, serde_json::to_vec(&sources).unwrap()).unwrap();
    BenchmarkOptions {
        manifest,
        sources: mappings,
        mode: "full_window".into(),
        capture_policy: CapturePolicy::ReuseValid,
        data_dir: root.join("evaluation"),
        output: root.join("output"),
        run_id: "run1".into(),
        pair_id: "p1".into(),
        variant: "candidate".into(),
        scenario: "cold".into(),
        allow_remote: false,
        deadline_ms: 5000,
    }
}

#[test]
fn existing_non_evaluation_directory_is_rejected_before_any_writes() {
    let root = tempfile::tempdir().unwrap();
    let opts = options(root.path(), &[1]);
    std::fs::create_dir_all(&opts.data_dir).unwrap();
    std::fs::write(opts.data_dir.join("app.db"), b"existing-instance").unwrap();
    assert!(BenchmarkSession::prepare(opts.clone()).is_err());
    assert_eq!(
        std::fs::read(opts.data_dir.join("app.db")).unwrap(),
        b"existing-instance"
    );
    assert_eq!(std::fs::read_dir(&opts.data_dir).unwrap().count(), 1);
}

#[test]
fn source_coordinates_are_required_and_intro_outro_must_agree() {
    let root = tempfile::tempdir().unwrap();
    let opts = options(root.path(), &[1]);
    let mut rows: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&opts.sources).unwrap()).unwrap();
    rows[0].as_object_mut().unwrap().remove("unit");
    std::fs::write(&opts.sources, serde_json::to_vec(&rows).unwrap()).unwrap();
    assert!(BenchmarkSession::prepare(opts.clone()).is_err());
    assert!(!opts.data_dir.exists());
    let opts = options(root.path(), &[1]);
    let mut rows: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&opts.sources).unwrap()).unwrap();
    rows[1]["unit"]["episode"] = json!(2);
    std::fs::write(&opts.sources, serde_json::to_vec(&rows).unwrap()).unwrap();
    assert!(BenchmarkSession::prepare(opts).is_err());
}

#[test]
fn hash_failure_still_produces_an_error_prediction_for_each_case() {
    let root = tempfile::tempdir().unwrap();
    let opts = options(root.path(), &[1]);
    std::fs::write(root.path().join("episode-1.mp4"), b"changed source").unwrap();
    let run = BenchmarkSession::prepare(opts).unwrap().run().unwrap();
    assert_eq!(run.predictions.len(), 2);
    assert!(
        run.predictions
            .iter()
            .all(|prediction| prediction.state == "error")
    );
    assert!(!run.wallclock_complete);
    assert!(run.input_bytes.is_none());
    assert!(!run.measurement_complete);
}
