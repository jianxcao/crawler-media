//! Consolidated integration test suite to reduce redundant linker artifacts.
#![allow(clippy::duplicate_mod, clippy::module_inception)]

#[path = "adaptive_benchmark_report.rs"]
mod adaptive_benchmark_report;
#[path = "artwork_security.rs"]
mod artwork_security;
#[path = "browse_semantic_parity.rs"]
mod browse_semantic_parity;
#[path = "config_parsing.rs"]
mod config_parsing;
#[path = "downloader_remove_cancel.rs"]
mod downloader_remove_cancel;
#[path = "favorites_keyset.rs"]
mod favorites_keyset;
#[path = "fingerprint_job.rs"]
mod fingerprint_job;
#[path = "jellyfin_episode_artwork.rs"]
mod jellyfin_episode_artwork;
#[path = "jellyfin_query_binding.rs"]
mod jellyfin_query_binding;
#[path = "job_updates.rs"]
mod job_updates;
#[path = "jobs_reader_failures.rs"]
mod jobs_reader_failures;
#[path = "library_delete_cancellation.rs"]
mod library_delete_cancellation;
#[path = "library_path_ownership.rs"]
mod library_path_ownership;
#[path = "library_read_metrics.rs"]
mod library_read_metrics;
#[path = "library_removal_http.rs"]
mod library_removal_http;
#[path = "library_statistics_scale.rs"]
mod library_statistics_scale;
#[path = "maintenance_config_atomic.rs"]
mod maintenance_config_atomic;
#[path = "maintenance_runtime.rs"]
mod maintenance_runtime;
#[path = "media_artwork_urls.rs"]
mod media_artwork_urls;
#[path = "next_up_resume.rs"]
mod next_up_resume;
#[path = "playback_session_migration.rs"]
mod playback_session_migration;
#[path = "poster_worker_lifetime.rs"]
mod poster_worker_lifetime;
#[path = "protocol_person_images.rs"]
mod protocol_person_images;
#[path = "reconciliation_budget.rs"]
mod reconciliation_budget;
#[path = "rename_startup.rs"]
mod rename_startup;
#[path = "review_remediation_lifecycle.rs"]
mod review_remediation_lifecycle;
#[path = "scan_operations.rs"]
mod scan_operations;
#[path = "scan_statistics.rs"]
mod scan_statistics;
#[path = "search_snapshot_retention.rs"]
mod search_snapshot_retention;
#[path = "source_token_rename.rs"]
mod source_token_rename;
#[path = "storage_transfer_recovery.rs"]
mod storage_transfer_recovery;
#[path = "subscribe_compensation_race.rs"]
mod subscribe_compensation_race;
#[path = "subscription_coverage_atomicity.rs"]
mod subscription_coverage_atomicity;
#[path = "theintrodb_http.rs"]
mod theintrodb_http;
#[path = "unidentified_identity.rs"]
mod unidentified_identity;
#[path = "view_io_isolation.rs"]
mod view_io_isolation;
#[path = "writer_isolation.rs"]
mod writer_isolation;

fn collect_test_registrations(
    cargo_toml: &str,
    suites: &[String],
) -> std::collections::BTreeMap<String, usize> {
    let mut counts = std::collections::BTreeMap::new();
    let mut in_test = false;
    for raw_line in cargo_toml.lines() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line == "[[test]]" {
            in_test = true;
        } else if line.starts_with('[') {
            in_test = false;
        } else if in_test
            && let Some(rest) = line.strip_prefix("path")
            && let Some((_, rhs)) = rest.split_once('=')
        {
            let path = rhs.trim().trim_matches('"');
            let rel = path.strip_prefix("tests/").unwrap_or(path);
            *counts.entry(rel.to_string()).or_insert(0) += 1;
        }
    }
    for suite in suites {
        let mut pending_path: Option<String> = None;
        for raw_line in suite.lines() {
            let line = raw_line.split("//").next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("#[path")
                && let Some((_, quoted)) = rest.split_once('"')
                && let Some((p, after)) = quoted.split_once('"')
            {
                if after.contains("mod ") && after.ends_with(';') {
                    *counts.entry(p.to_string()).or_insert(0) += 1;
                    pending_path = None;
                } else {
                    pending_path = Some(p.to_string());
                }
                continue;
            }
            if let Some(rest) = line
                .strip_prefix("mod ")
                .or_else(|| line.strip_prefix("pub mod "))
                && let Some(ident) = rest.strip_suffix(';')
            {
                let target = pending_path
                    .take()
                    .unwrap_or_else(|| format!("{}.rs", ident.trim()));
                *counts.entry(target).or_insert(0) += 1;
            } else if !line.starts_with("#[") {
                pending_path = None;
            }
        }
    }
    counts
}

#[test]
fn every_top_level_api_test_file_is_registered() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let cargo_toml = std::fs::read_to_string(manifest_dir.join("Cargo.toml")).unwrap();
    let suites: Vec<String> = (1..=4)
        .map(|i| {
            std::fs::read_to_string(manifest_dir.join(format!("tests/api_suite_{i}.rs"))).unwrap()
        })
        .collect();
    let counts = collect_test_registrations(&cargo_toml, &suites);
    let mut targets = Vec::new();
    for entry in std::fs::read_dir(manifest_dir.join("tests")).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy();
        if path.is_dir() && path.join("main.rs").is_file() {
            targets.push(format!("{name}/main.rs"));
        } else if path.is_file()
            && path.extension().and_then(|e| e.to_str()) == Some("rs")
            && name != "probe_retry_support.rs"
            && !name.starts_with("api_suite_")
        {
            targets.push(name.into_owned());
        }
    }
    targets.sort();
    let mut missing = Vec::new();
    let mut duplicates = Vec::new();
    for target in targets {
        match counts.get(&target).copied().unwrap_or(0) {
            0 => missing.push(target),
            1 => {}
            n => duplicates.push(format!("{target} ({n}x)")),
        }
    }
    assert!(
        missing.is_empty(),
        "Unregistered integration test targets (add to Cargo.toml or api_suite_1..4.rs): {missing:?}"
    );
    assert!(
        duplicates.is_empty(),
        "Duplicate integration test registrations across Cargo.toml and api_suite_1..4.rs: {duplicates:?}"
    );
}
