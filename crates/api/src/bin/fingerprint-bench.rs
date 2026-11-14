use api::fingerprint_job::{
    CapturePolicy,
    benchmark::{BenchmarkOptions, BenchmarkSession},
};
use std::{collections::BTreeMap, path::PathBuf};

fn main() {
    if let Err(error) = run() {
        eprintln!("fingerprint-bench: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut values = BTreeMap::new();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        if flag == "--help" {
            println!(
                "fingerprint-bench --manifest PATH --sources PATH --mode full_window|adaptive --capture-policy reuse_valid|recapture --data-dir PATH --output PATH --run-id ID [--scenario cold|warm|incremental|refresh|retry] [--deadline-ms N] [--allow-remote]"
            );
            return Ok(());
        }
        if flag == "--allow-remote" {
            values.insert(flag, "true".into());
            continue;
        }
        if !matches!(
            flag.as_str(),
            "--manifest"
                | "--sources"
                | "--mode"
                | "--capture-policy"
                | "--data-dir"
                | "--output"
                | "--run-id"
                | "--scenario"
                | "--deadline-ms"
                | "--pair-id"
                | "--variant"
        ) {
            return Err(format!("unknown argument: {flag}"));
        }
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        if values.insert(flag.clone(), value).is_some() {
            return Err(format!("duplicate argument: {flag}"));
        }
    }
    let required = |key: &str| {
        values
            .get(key)
            .cloned()
            .ok_or_else(|| format!("required argument: {key}"))
    };
    let policy = match required("--capture-policy")?.as_str() {
        "reuse_valid" => CapturePolicy::ReuseValid,
        "recapture" => CapturePolicy::Recapture,
        _ => return Err("capture policy must be reuse_valid or recapture".into()),
    };
    let run_id = required("--run-id")?;
    let options = BenchmarkOptions {
        manifest: PathBuf::from(required("--manifest")?),
        sources: PathBuf::from(required("--sources")?),
        mode: required("--mode")?,
        capture_policy: policy,
        data_dir: PathBuf::from(required("--data-dir")?),
        output: PathBuf::from(required("--output")?),
        pair_id: values
            .get("--pair-id")
            .cloned()
            .unwrap_or_else(|| run_id.clone()),
        run_id,
        variant: values
            .get("--variant")
            .cloned()
            .unwrap_or_else(|| "candidate".into()),
        scenario: values
            .get("--scenario")
            .cloned()
            .unwrap_or_else(|| "refresh".into()),
        allow_remote: values.contains_key("--allow-remote"),
        deadline_ms: values
            .get("--deadline-ms")
            .map(|value| value.parse::<u64>())
            .transpose()
            .map_err(|e| e.to_string())?
            .unwrap_or(3_600_000),
    };
    let run = BenchmarkSession::prepare(options)?.run()?;
    for media in &run.media {
        println!(
            "{}: {} episodes, {} files, {:.3}s, {:?}, frames {}/{}; mode={}, capture_policy={:?}, scenario={}",
            media.media_group,
            media.independent_episode_count,
            media.file_count,
            media.elapsed_ms as f64 / 1000.,
            media.terminal_status,
            media.ready_frames,
            media.frame_targets,
            run.mode,
            run.capture_policy,
            run.scenario
        );
    }
    if !run.wallclock_complete {
        return Err(format!(
            "lifecycle {:?}; diagnostics and predictions saved",
            run.terminal_status
        ));
    }
    Ok(())
}
