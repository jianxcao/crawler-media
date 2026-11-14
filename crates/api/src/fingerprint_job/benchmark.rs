//! Isolated evaluation using the production probe and frame lifecycle.
mod types;
pub use types::*;
mod input;
mod lifecycle;
mod output;
mod provenance;
mod registration;

use super::CapturePolicy;
use crate::ScrapeStoreExt;
use std::sync::Arc;

pub struct BenchmarkSession {
    pub state: crate::ApiState,
    options: BenchmarkOptions,
    manifest: BenchmarkManifest,
    inputs: Vec<input::VerifiedInput>,
    registered: Vec<registration::RegisteredSource>,
    build: BuildProvenance,
}

struct NoNetwork;
impl indexer::Fetcher for NoNetwork {
    fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        Err(indexer::IndexerError::Parse(
            "network disabled in fingerprint evaluation".into(),
        ))
    }
}

impl BenchmarkSession {
    /// Call outside a Tokio runtime; only probe/frame workers are started by run().
    pub fn prepare(options: BenchmarkOptions) -> Result<Self, String> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err("prepare evaluation outside an async runtime".into());
        }
        if !matches!(options.mode.as_str(), "full_window" | "adaptive")
            || !matches!(
                options.scenario.as_str(),
                "cold" | "warm" | "incremental" | "refresh" | "retry"
            )
            || options.variant != "candidate"
            || options.run_id.is_empty()
            || !options
                .run_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            || !options.output.is_absolute()
            || options.pair_id.trim().is_empty()
            || options.deadline_ms == 0
        {
            return Err("invalid current-version evaluation options".into());
        }
        if matches!(options.scenario.as_str(), "incremental" | "retry")
            && options.capture_policy != CapturePolicy::ReuseValid
        {
            return Err(format!(
                "{} uses natural production cache reuse and requires capture_policy reuse_valid",
                options.scenario
            ));
        }
        let (manifest, mut inputs) = input::load(&options)?;
        input::evaluation_directory(&options.data_dir)?;
        let store = Arc::new(crate::Store::open(&options.data_dir).map_err(|e| e.to_string())?);
        let registered = registration::register(&store, &mut inputs, &options.data_dir)?;
        let mut settings = store
            .get_scrape_config()
            .map_err(|e| e.to_string())?
            .setting;
        settings.fingerprint_sampling_mode = Some(options.mode.clone());
        settings.fingerprint_duration_secs = Some(180);
        store
            .save_scrape_config(&settings)
            .map_err(|e| e.to_string())?;
        let state = crate::ApiState::new_arc(
            store,
            uuid::Uuid::new_v4().to_string(),
            indexer::ProfileSet::load(None).map_err(|e| e.to_string())?,
            Arc::new(NoNetwork),
            Arc::new(downloader::MemoryDownloader::new(
                options.data_dir.join("download-stage"),
            )),
        )
        .map_err(|e| e.to_string())?;
        Ok(Self {
            state,
            options,
            manifest,
            inputs,
            registered,
            build: provenance::build(),
        })
    }

    pub fn registered_files(&self) -> Vec<domain::FileCoverage> {
        self.registered
            .iter()
            .map(|input| input.row.clone())
            .collect()
    }

    pub fn run(self) -> Result<BenchmarkRun, String> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let result = runtime.block_on(lifecycle::execute(self));
        runtime.shutdown_timeout(std::time::Duration::from_secs(5));
        result
    }
}
