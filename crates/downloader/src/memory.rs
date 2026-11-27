use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use domain::Torrent;

use crate::{
    ClientSubmitResult, ClientTaskQuery, DiskSpaceInfo, DiskSpaceStatus, Downloader,
    DownloaderError, SubmitOptions, SubmitPayload, TaskFileSet, TaskSnapshot,
};

#[derive(Clone, Debug)]
pub struct MemorySubmission {
    pub payload: SubmitPayload,
    pub options: SubmitOptions,
}

#[derive(Clone, Debug, Default)]
struct MemoryTaskMeta {
    files: Vec<PathBuf>,
    total_bytes: u64,
    tags: Vec<String>,
    category: Option<String>,
    upload_limit: Option<u64>,
    download_limit: Option<u64>,
    claimed_by_app: bool,
}

pub struct MemoryDownloader {
    stage: PathBuf,
    maps: Mutex<HashMap<String, Vec<PathBuf>>>,
    added: Mutex<Vec<Torrent>>,
    submitted: Mutex<Vec<MemorySubmission>>,
    tasks: Mutex<HashMap<String, (String, bool, bool)>>,
    task_meta: Mutex<HashMap<String, MemoryTaskMeta>>,
    identities: Mutex<HashMap<String, String>>,
    foreign_hashes: Mutex<HashSet<String>>,
    broken_hashes: Mutex<HashSet<String>>,
    force_unknown: Mutex<bool>,
    requested: Mutex<Vec<String>>,
    removed: Mutex<Vec<String>>,
    deleted_calls: Mutex<Vec<(String, bool)>>,
    destinations: Mutex<Vec<Option<String>>>,
    snapshots: Mutex<Vec<TaskSnapshot>>,
    disk_space: Mutex<(DiskSpaceStatus, Option<u64>)>,
    disk_error: Mutex<bool>,
}

impl MemoryDownloader {
    pub fn new(stage: impl AsRef<Path>) -> Self {
        let stage = stage.as_ref().to_path_buf();
        fs::create_dir_all(&stage).ok();
        Self {
            stage,
            maps: Mutex::new(HashMap::new()),
            added: Mutex::new(Vec::new()),
            submitted: Mutex::new(Vec::new()),
            tasks: Mutex::new(HashMap::new()),
            task_meta: Mutex::new(HashMap::new()),
            identities: Mutex::new(HashMap::new()),
            foreign_hashes: Mutex::new(HashSet::new()),
            broken_hashes: Mutex::new(HashSet::new()),
            force_unknown: Mutex::new(false),
            requested: Mutex::new(Vec::new()),
            removed: Mutex::new(Vec::new()),
            deleted_calls: Mutex::new(Vec::new()),
            destinations: Mutex::new(Vec::new()),
            snapshots: Mutex::new(Vec::new()),
            disk_space: Mutex::new((DiskSpaceStatus::Available, Some(100 * 1024 * 1024 * 1024))),
            disk_error: Mutex::new(false),
        }
    }

    pub fn set_disk_space(&self, status: DiskSpaceStatus, available_bytes: Option<u64>) {
        *self.disk_space.lock() = (status, available_bytes);
    }

    pub fn set_disk_error(&self, value: bool) {
        *self.disk_error.lock() = value;
    }

    pub fn set_task_files(&self, info_hash: &str, files: Vec<PathBuf>, total_bytes: u64) {
        let key = info_hash.trim().to_ascii_lowercase();
        let mut guard = self.task_meta.lock();
        let entry = guard.entry(key).or_default();
        entry.files = files;
        entry.total_bytes = total_bytes;
    }

    pub fn set_task_claimed(&self, info_hash: &str, claimed: bool) {
        let key = info_hash.trim().to_ascii_lowercase();
        self.task_meta.lock().entry(key).or_default().claimed_by_app = claimed;
    }

    pub fn is_task_paused(&self, info_hash: &str) -> bool {
        let key = info_hash.trim().to_ascii_lowercase();
        self.tasks.lock().get(&key).is_some_and(|t| t.2)
    }

    pub fn task_tags(&self, info_hash: &str) -> Vec<String> {
        let key = info_hash.trim().to_ascii_lowercase();
        self.task_meta
            .lock()
            .get(&key)
            .map(|m| m.tags.clone())
            .unwrap_or_default()
    }

    pub fn task_category(&self, info_hash: &str) -> Option<String> {
        let key = info_hash.trim().to_ascii_lowercase();
        self.task_meta
            .lock()
            .get(&key)
            .and_then(|m| m.category.clone())
    }

    pub fn task_limits(&self, info_hash: &str) -> (Option<u64>, Option<u64>) {
        let key = info_hash.trim().to_ascii_lowercase();
        self.task_meta
            .lock()
            .get(&key)
            .map(|m| (m.upload_limit, m.download_limit))
            .unwrap_or((None, None))
    }

    pub fn deleted_tasks(&self) -> Vec<(String, bool)> {
        self.deleted_calls.lock().clone()
    }

    pub fn map_enclosure(&self, enclosure: &str, src: PathBuf) {
        self.map_enclosure_files(enclosure, vec![src]);
    }

    pub fn map_enclosure_files(&self, enclosure: &str, sources: Vec<PathBuf>) {
        self.maps.lock().insert(enclosure.to_string(), sources);
    }

    pub fn added(&self) -> Vec<Torrent> {
        self.added.lock().clone()
    }

    pub fn submitted(&self) -> Vec<MemorySubmission> {
        self.submitted.lock().clone()
    }

    pub fn inject_foreign_hash(&self, info_hash: &str) {
        self.foreign_hashes
            .lock()
            .insert(info_hash.trim().to_ascii_lowercase());
    }

    pub fn inject_broken_hash(&self, info_hash: &str) {
        self.broken_hashes
            .lock()
            .insert(info_hash.trim().to_ascii_lowercase());
    }

    pub fn set_force_unknown(&self, value: bool) {
        *self.force_unknown.lock() = value;
    }

    /// 被 remove 过的种子标题（测试断言用）。
    pub fn removed(&self) -> Vec<String> {
        self.removed.lock().clone()
    }

    pub fn destinations(&self) -> Vec<Option<String>> {
        self.destinations.lock().clone()
    }

    pub fn push_snapshot(&self, snapshot: TaskSnapshot) {
        self.snapshots.lock().push(snapshot);
    }

    fn check_existing_memory_task(
        &self,
        info_hash: &str,
        meta_tags: &[String],
        options: &SubmitOptions,
    ) -> Option<ClientSubmitResult> {
        let outcome = {
            let mut tasks = self.tasks.lock();
            let (tag, broken, paused) = tasks.get_mut(info_hash)?;
            let expected_tag = options.ownership_tag.trim();
            let exact_owned =
                tag.trim() == expected_tag || meta_tags.iter().any(|t| t.trim() == expected_tag);
            let instance_owned = exact_owned
                || crate::is_instance_owned_tag(tag, expected_tag)
                || meta_tags
                    .iter()
                    .any(|t| crate::is_instance_owned_tag(t, expected_tag));
            if !instance_owned {
                ClientSubmitResult::ForeignDuplicate {
                    info_hash: info_hash.to_string(),
                }
            } else if *broken {
                ClientSubmitResult::ExistingBroken {
                    info_hash: info_hash.to_string(),
                }
            } else if exact_owned {
                if !options.paused {
                    *paused = false;
                }
                ClientSubmitResult::Added {
                    already_owned: true,
                    info_hash: info_hash.to_string(),
                }
            } else {
                ClientSubmitResult::ForeignDuplicate {
                    info_hash: info_hash.to_string(),
                }
            }
        };
        if matches!(
            outcome,
            ClientSubmitResult::Added {
                already_owned: true,
                ..
            }
        ) {
            let mut meta = self.task_meta.lock();
            let entry = meta.entry(info_hash.to_string()).or_default();
            for extra in &options.extra_tags {
                let trimmed = extra.trim();
                if !trimmed.is_empty() && !entry.tags.iter().any(|t| t == trimmed) {
                    entry.tags.push(trimmed.to_string());
                }
            }
        }
        Some(outcome)
    }

    fn resolve_task_hash(&self, torrent: &Torrent) -> Option<String> {
        if let Some(hash) = self.identities.lock().get(&torrent.enclosure).cloned() {
            return Some(hash);
        }
        let enc_tag = crate::ownership_tag(&torrent.enclosure);
        if let Some(hash) = self
            .task_meta
            .lock()
            .iter()
            .find_map(|(h, meta)| meta.tags.iter().any(|t| t == &enc_tag).then(|| h.clone()))
        {
            return Some(hash);
        }
        self.tasks
            .lock()
            .iter()
            .find_map(|(h, (tag, _, _))| (tag.trim() == enc_tag).then(|| h.clone()))
    }

    fn purge_torrent_task(&self, torrent: &Torrent) {
        let resolved = self
            .resolve_task_hash(torrent)
            .or_else(|| crate::magnet_info_hash(&torrent.enclosure));
        let enc_tag = crate::ownership_tag(&torrent.enclosure);
        self.identities.lock().remove(&torrent.enclosure);
        self.added.lock().retain(|t| {
            t.enclosure != torrent.enclosure && crate::ownership_tag(&t.enclosure) != enc_tag
        });
        self.requested.lock().retain(|r| r != &torrent.enclosure);
        if let Some(hash) = resolved {
            let normalized = hash.trim().to_ascii_lowercase();
            self.tasks.lock().remove(&normalized);
            self.task_meta.lock().remove(&normalized);
            self.identities.lock().retain(|_, h| h != &normalized);
        }
    }

    fn stage_copies(&self, enclosure: &str) -> Result<(), DownloaderError> {
        if !self.requested.lock().iter().any(|item| item == enclosure) {
            return Ok(());
        }
        let maps = self.maps.lock();
        let Some(sources) = maps.get(enclosure) else {
            return Ok(());
        };
        for src in sources.iter().filter(|src| src.exists()) {
            let dest = self
                .stage
                .join(src.file_name().unwrap_or_else(|| "file.bin".as_ref()));
            if !dest.exists() {
                fs::copy(src, dest)?;
            }
        }
        Ok(())
    }
}

fn staged_files(
    stage: &Path,
    maps: &HashMap<String, Vec<PathBuf>>,
    enclosure: &str,
) -> Vec<PathBuf> {
    let Some(sources) = maps.get(enclosure) else {
        return Vec::new();
    };
    sources
        .iter()
        .map(|src| stage.join(src.file_name().unwrap_or_else(|| "file.bin".as_ref())))
        .filter(|dest| dest.exists())
        .collect()
}

impl Downloader for MemoryDownloader {
    fn remember_identity(&self, torrent: &Torrent, hash: &str) -> Result<(), DownloaderError> {
        let mut added = self.added.lock();
        if !added.iter().any(|t| t.enclosure == torrent.enclosure) {
            added.push(torrent.clone());
        }
        drop(added);
        {
            let mut req = self.requested.lock();
            if !req.iter().any(|r| r == &torrent.enclosure) {
                req.push(torrent.enclosure.clone());
            }
        }
        self.stage_copies(&torrent.enclosure)?;
        let normalized = hash.trim().to_ascii_lowercase();
        if !normalized.is_empty() {
            self.identities
                .lock()
                .insert(torrent.enclosure.clone(), normalized);
        }
        Ok(())
    }

    fn submit_verified(
        &self,
        payload: &SubmitPayload,
        options: &SubmitOptions,
    ) -> Result<ClientSubmitResult, DownloaderError> {
        if options.ownership_tag.contains(',')
            || options.ownership_tag.chars().any(char::is_control)
        {
            return Err(DownloaderError::Message("invalid ownership tag".into()));
        }
        for extra in &options.extra_tags {
            if extra.contains(',') || extra.chars().any(char::is_control) {
                return Err(DownloaderError::Message(format!("invalid tag `{extra}`")));
            }
        }
        let info_hash = payload.info_hash().trim().to_ascii_lowercase();
        if *self.force_unknown.lock() {
            return Ok(ClientSubmitResult::UnknownOutcome {
                reason: "simulated unknown outcome".into(),
            });
        }
        if self.foreign_hashes.lock().contains(&info_hash) {
            return Ok(ClientSubmitResult::ForeignDuplicate { info_hash });
        }
        if self.broken_hashes.lock().contains(&info_hash) {
            return Ok(ClientSubmitResult::ExistingBroken { info_hash });
        }
        let meta_tags = self
            .task_meta
            .lock()
            .get(&info_hash)
            .map(|m| m.tags.clone())
            .unwrap_or_default();
        if let Some(outcome) = self.check_existing_memory_task(&info_hash, &meta_tags, options) {
            return Ok(outcome);
        }
        self.destinations.lock().push(options.save_path.clone());
        if let Some(cat) = options.category.as_deref()
            && let Err(reason) = crate::validate_category_name(cat)
        {
            return Err(DownloaderError::Message(reason.into()));
        }
        self.submitted.lock().push(MemorySubmission {
            payload: payload.clone(),
            options: options.clone(),
        });
        self.tasks.lock().insert(
            info_hash.clone(),
            (options.ownership_tag.clone(), false, options.paused),
        );
        self.task_meta.lock().insert(
            info_hash.clone(),
            MemoryTaskMeta {
                files: Vec::new(),
                total_bytes: 0,
                tags: options.extra_tags.clone(),
                category: options.category.clone(),
                upload_limit: options.upload_limit,
                download_limit: options.download_limit,
                claimed_by_app: false,
            },
        );
        self.requested.lock().push(info_hash.clone());
        let _ = self.stage_copies(&info_hash);
        Ok(ClientSubmitResult::Added {
            already_owned: false,
            info_hash,
        })
    }

    fn query_task_status(
        &self,
        info_hash: &str,
        ownership_tag: &str,
    ) -> Result<ClientTaskQuery, DownloaderError> {
        let normalized = info_hash.trim().to_ascii_lowercase();
        if self.broken_hashes.lock().contains(&normalized) {
            return Ok(ClientTaskQuery {
                exists: true,
                info_hash: Some(normalized),
                owned_by_tag: false,
                broken: true,
                state: Some("error".into()),
                paused: false,
            });
        }
        if self.foreign_hashes.lock().contains(&normalized) {
            return Ok(ClientTaskQuery {
                exists: true,
                info_hash: Some(normalized),
                owned_by_tag: false,
                broken: false,
                state: Some("downloading".into()),
                paused: false,
            });
        }
        if let Some((tag, broken, paused)) = self.tasks.lock().get(&normalized) {
            return Ok(ClientTaskQuery {
                exists: true,
                info_hash: Some(normalized),
                owned_by_tag: !ownership_tag.is_empty() && tag == ownership_tag,
                broken: *broken,
                state: Some(if *paused { "pausedDL" } else { "downloading" }.into()),
                paused: *paused,
            });
        }
        Ok(ClientTaskQuery::default())
    }

    fn remove(&self, torrent: &Torrent, _delete_files: bool) -> Result<(), DownloaderError> {
        self.removed.lock().push(torrent.title.clone());
        self.purge_torrent_task(torrent);
        Ok(())
    }

    fn owned_identity(&self, torrent: &Torrent) -> Result<Option<String>, DownloaderError> {
        if let Some(hash) = self.resolve_task_hash(torrent) {
            return Ok(Some(hash));
        }
        Ok(Some(
            crate::magnet_info_hash(&torrent.enclosure)
                .unwrap_or_else(|| format!("memory:{}", torrent.enclosure)),
        ))
    }

    fn remove_owned(&self, torrent: &Torrent, _delete_files: bool) -> Result<(), DownloaderError> {
        self.removed.lock().push(torrent.title.clone());
        self.purge_torrent_task(torrent);
        Ok(())
    }

    fn delete_task(&self, info_hash: &str, delete_files: bool) -> Result<(), DownloaderError> {
        let normalized = info_hash.trim().to_ascii_lowercase();
        tracing::info!(hash = %normalized, delete_files, "从内存下载器删除任务");
        self.removed.lock().push(info_hash.to_string());
        self.deleted_calls
            .lock()
            .push((normalized.clone(), delete_files));
        let removed_task = self.tasks.lock().remove(&normalized);
        self.task_meta.lock().remove(&normalized);
        let mut removed_enclosures = Vec::new();
        self.identities.lock().retain(|enc, h| {
            if h == &normalized {
                removed_enclosures.push(enc.clone());
                false
            } else {
                true
            }
        });
        let removed_owner_tag = removed_task.map(|(tag, _, _)| tag);
        self.added.lock().retain(|t| {
            !removed_enclosures.contains(&t.enclosure)
                && crate::magnet_info_hash(&t.enclosure).as_deref() != Some(normalized.as_str())
                && removed_owner_tag.as_deref() != Some(crate::ownership_tag(&t.enclosure).as_str())
        });
        self.requested
            .lock()
            .retain(|r| r != &normalized && !removed_enclosures.contains(r));
        Ok(())
    }

    fn pause_task(&self, info_hash: &str) -> Result<(), DownloaderError> {
        let normalized = info_hash.trim().to_ascii_lowercase();
        let mut guard = self.tasks.lock();
        let Some(entry) = guard.get_mut(&normalized) else {
            return Err(DownloaderError::Message(format!(
                "task {normalized} not found"
            )));
        };
        entry.2 = true;
        Ok(())
    }

    fn resume_task(&self, info_hash: &str) -> Result<(), DownloaderError> {
        let normalized = info_hash.trim().to_ascii_lowercase();
        let mut guard = self.tasks.lock();
        let Some(entry) = guard.get_mut(&normalized) else {
            return Err(DownloaderError::Message(format!(
                "task {normalized} not found"
            )));
        };
        entry.2 = false;
        Ok(())
    }

    fn task_files_by_hash(&self, info_hash: &str) -> Result<Option<TaskFileSet>, DownloaderError> {
        let normalized = info_hash.trim().to_ascii_lowercase();
        if !self.tasks.lock().contains_key(&normalized) {
            return Ok(None);
        }
        let meta = self
            .task_meta
            .lock()
            .get(&normalized)
            .cloned()
            .unwrap_or_default();
        Ok(Some(TaskFileSet {
            save_path: self.stage.to_string_lossy().into_owned(),
            files: meta.files,
            total_bytes: meta.total_bytes,
            claimed_by_app: meta.claimed_by_app,
            tags: meta.tags,
            category: meta.category,
        }))
    }

    fn set_task_tags(&self, info_hash: &str, tags: &[String]) -> Result<(), DownloaderError> {
        for raw in tags {
            if raw.contains(',') || raw.chars().any(char::is_control) {
                return Err(DownloaderError::Message(format!("invalid tag `{raw}`")));
            }
        }
        let normalized = info_hash.trim().to_ascii_lowercase();
        let Some(owner_tag) = self
            .tasks
            .lock()
            .get(&normalized)
            .map(|(tag, _, _)| tag.trim().to_string())
        else {
            return Err(DownloaderError::Message(format!(
                "task {normalized} not found"
            )));
        };
        let mut guard = self.task_meta.lock();
        let entry = guard.entry(normalized).or_default();
        let is_sys = |t: &str| crate::is_reserved_system_tag(t);
        for tag in tags {
            let trimmed = tag.trim();
            if trimmed.is_empty() {
                continue;
            }
            let present = entry.tags.iter().any(|t| t == trimmed)
                || (!owner_tag.is_empty() && owner_tag == trimmed)
                || (entry.claimed_by_app && trimmed == crate::CLAIMED_TAG);
            if is_sys(trimmed) && !present {
                return Err(DownloaderError::Message(format!(
                    "cannot inject unowned system tag `{trimmed}`"
                )));
            }
        }
        let mut merged: Vec<String> = entry.tags.iter().filter(|t| is_sys(t)).cloned().collect();
        for tag in tags {
            let trimmed = tag.trim();
            if !trimmed.is_empty() && !merged.iter().any(|m| m == trimmed) {
                merged.push(trimmed.to_string());
            }
        }
        entry.tags = merged;
        Ok(())
    }

    fn set_task_category(&self, info_hash: &str, category: &str) -> Result<(), DownloaderError> {
        if let Err(reason) = crate::validate_category_name(category) {
            return Err(DownloaderError::Message(reason.into()));
        }
        let normalized = info_hash.trim().to_ascii_lowercase();
        if !self.tasks.lock().contains_key(&normalized) {
            return Err(DownloaderError::Message(format!(
                "task {normalized} not found"
            )));
        }
        self.task_meta
            .lock()
            .entry(normalized)
            .or_default()
            .category = Some(category.to_string());
        Ok(())
    }

    fn set_task_limits(
        &self,
        info_hash: &str,
        upload_limit: Option<u64>,
        download_limit: Option<u64>,
    ) -> Result<(), DownloaderError> {
        let normalized = info_hash.trim().to_ascii_lowercase();
        if !self.tasks.lock().contains_key(&normalized) {
            return Err(DownloaderError::Message(format!(
                "task {normalized} not found"
            )));
        }
        let mut guard = self.task_meta.lock();
        let entry = guard.entry(normalized).or_default();
        entry.upload_limit = upload_limit;
        entry.download_limit = download_limit;
        Ok(())
    }

    fn disk_free_space(&self) -> Result<DiskSpaceInfo, DownloaderError> {
        if *self.disk_error.lock() {
            return Err(DownloaderError::Message("simulated disk error".into()));
        }
        let (status, available_bytes) = *self.disk_space.lock();
        Ok(DiskSpaceInfo {
            available_bytes,
            root_path: Some(self.stage.clone()),
            status,
        })
    }

    fn endpoint(&self) -> Option<String> {
        Some("memory|memory".into())
    }

    fn completed_files(&self, torrent: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        self.stage_copies(&torrent.enclosure)?;
        let staged = staged_files(&self.stage, &self.maps.lock(), &torrent.enclosure);
        if !staged.is_empty() {
            return Ok(staged);
        }
        Ok(self
            .maps
            .lock()
            .get(&torrent.enclosure)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|src| src.exists())
            .collect())
    }

    fn task_snapshots(&self) -> Result<Vec<TaskSnapshot>, DownloaderError> {
        let injected = self.snapshots.lock().clone();
        if !injected.is_empty() {
            return Ok(injected);
        }
        let tasks = self.tasks.lock().clone();
        let task_meta = self.task_meta.lock().clone();
        let identities = self.identities.lock().clone();
        let added = self.added.lock().clone();
        let mut rows = Vec::with_capacity(tasks.len() + added.len());
        for (hash, (ownership_tag, broken, paused)) in &tasks {
            let meta = task_meta.get(hash);
            let mut extra_owned: Vec<String> = vec![ownership_tag.clone()];
            if let Some(m) = meta {
                extra_owned.extend(m.tags.iter().cloned());
            }
            let mut title = hash.clone();
            let mut fallback_size = 0;
            for (enc, mapped_hash) in &identities {
                if mapped_hash == hash {
                    extra_owned.push(crate::ownership_tag(enc));
                    if let Some(t) = added.iter().find(|t| &t.enclosure == enc) {
                        title = t.title.clone();
                        fallback_size = t.size_bytes.unwrap_or(0);
                    }
                }
            }
            let size_bytes = meta
                .map(|m| m.total_bytes)
                .filter(|&b| b > 0)
                .unwrap_or(fallback_size);
            rows.push(TaskSnapshot {
                tag: crate::TASK_TAG.to_string(),
                name: title,
                progress: 0.0,
                state: if *broken {
                    "error".into()
                } else if *paused {
                    "pausedDL".into()
                } else {
                    "downloading".into()
                },
                size_bytes,
                downloaded_bytes: 0,
                uploaded_bytes: 0,
                download_speed: 0,
                upload_speed: 0,
                info_hash: hash.clone(),
                owned_tags: crate::owned_tags_from(extra_owned.iter().map(String::as_str)),
            });
        }
        for torrent in added {
            let enc_tag = crate::ownership_tag(&torrent.enclosure);
            let represented = identities
                .get(&torrent.enclosure)
                .is_some_and(|h| tasks.contains_key(h))
                || crate::magnet_info_hash(&torrent.enclosure)
                    .is_some_and(|h| tasks.contains_key(&h))
                || rows
                    .iter()
                    .any(|r| r.owned_tags.iter().any(|t| t == &enc_tag));
            if represented {
                continue;
            }
            rows.push(TaskSnapshot {
                tag: crate::TASK_TAG.to_string(),
                name: torrent.title,
                progress: 0.0,
                state: "queued".into(),
                size_bytes: torrent.size_bytes.unwrap_or(0),
                downloaded_bytes: 0,
                uploaded_bytes: 0,
                download_speed: 0,
                upload_speed: 0,
                info_hash: String::new(),
                owned_tags: crate::owned_tags_from([enc_tag.as_str()]),
            });
        }
        Ok(rows)
    }
}
