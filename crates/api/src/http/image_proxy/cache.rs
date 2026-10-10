//! Serialized cache publication: reserve disk space before creating any file.
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub(super) type Image = (String, Vec<u8>);
type Remover = Arc<dyn Fn(&Path) -> std::io::Result<()> + Send + Sync>;

pub(super) struct ImageCache {
    root: PathBuf,
    budget: u64,
    response_limit: u64,
    publication: Mutex<()>,
    remove: Remover,
}

impl ImageCache {
    pub(super) fn new(root: PathBuf, budget: u64, response_limit: u64) -> Self {
        Self::with_remover(
            root,
            budget,
            response_limit,
            Arc::new(|path| fs::remove_file(path)),
        )
    }

    pub(super) fn with_remover(
        root: PathBuf,
        budget: u64,
        response_limit: u64,
        remove: Remover,
    ) -> Self {
        // Resolve relative configured data directories once; deletion guards require
        // an absolute root and must not depend on later working-directory changes.
        let root = std::path::absolute(&root).unwrap_or(root);
        Self {
            root,
            budget,
            response_limit,
            publication: Mutex::new(()),
            remove,
        }
    }

    pub(super) fn get(
        &self,
        url: &str,
        fetch: impl FnOnce() -> Result<Image, String>,
    ) -> Result<Image, String> {
        self.get_inner(url, fetch).map_err(|error| {
            tracing::error!(%error, root = %self.root.display(), "Image proxy cache operation failed");
            error
        })
    }

    fn get_inner(
        &self,
        url: &str,
        fetch: impl FnOnce() -> Result<Image, String>,
    ) -> Result<Image, String> {
        let path = self.root.join(format!("{}.img", super::cache_key(url)));
        {
            let _guard = self.publication.lock().map_err(|e| e.to_string())?;
            fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
            self.reserve(0)?;
            if let Some(image) = self.read(&path)? {
                return Ok(image);
            }
        }
        let (content_type, bytes) = fetch()?;
        if bytes.len() as u64 > self.response_limit {
            return Err(format!(
                "Image response exceeds {} bytes",
                self.response_limit
            ));
        }
        if !content_type.starts_with("image/")
            || content_type.contains(['\r', '\n'])
            || content_type.len() > 1024
        {
            return Err("Invalid image content type".into());
        }
        let _guard = self.publication.lock().map_err(|e| e.to_string())?;
        self.reserve(0)?;
        if let Some(image) = self.read(&path)? {
            return Ok(image);
        }
        let size = content_type.len() as u64 + 1 + bytes.len() as u64;
        self.reserve(size)?;
        let temp = self.root.join(format!("{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> std::io::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(content_type.as_bytes())?;
            file.write_all(b"\n")?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temp, &path)
        })();
        if let Err(error) = result {
            if temp.exists() {
                self.remove_checked(&temp)?;
            }
            return Err(format!("Image cache publication failed: {error}"));
        }
        tracing::debug!(path = %path.display(), size, "Image cache published");
        Ok((content_type, bytes))
    }

    fn read(&self, path: &Path) -> Result<Option<Image>, String> {
        let file = match fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        let mut payload = Vec::new();
        file.take(self.response_limit + 1026)
            .read_to_end(&mut payload)
            .map_err(|e| e.to_string())?;
        if let Some(split) = payload.iter().position(|b| *b == b'\n') {
            if split <= 1024 && payload.len() - split - 1 <= self.response_limit as usize {
                if let Ok(kind) = std::str::from_utf8(&payload[..split]) {
                    if kind.starts_with("image/") && !kind.contains('\r') {
                        return Ok(Some((kind.into(), payload[split + 1..].to_vec())));
                    }
                }
            }
        }
        tracing::warn!(path = %path.display(), "Removing invalid or oversized image cache entry");
        self.remove_checked(path)?;
        Ok(None)
    }

    fn reserve(&self, incoming: u64) -> Result<(), String> {
        if incoming > self.budget {
            return Err("Image exceeds disk cache budget".into());
        }
        let mut files = Vec::new();
        let mut total = 0u64;
        for entry in fs::read_dir(&self.root).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let meta = fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
            if meta.file_type().is_symlink() {
                return Err("Image cache contains a symlink".into());
            }
            if meta.is_file() {
                total = total.saturating_add(meta.len());
                files.push((
                    meta.modified().map_err(|e| e.to_string())?,
                    meta.len(),
                    entry.path(),
                ));
            }
        }
        files.sort_by_key(|(time, _, _)| *time);
        for (_, size, path) in files {
            if total <= self.budget - incoming {
                break;
            }
            self.remove_checked(&path)?;
            total = total.saturating_sub(size);
            tracing::debug!(path = %path.display(), size, "Image cache evicted to reserve space");
        }
        if total > self.budget - incoming {
            return Err("Cannot reserve image cache space".into());
        }
        Ok(())
    }

    fn remove_checked(&self, path: &Path) -> Result<(), String> {
        if path.parent() != Some(self.root.as_path()) || !self.root.is_absolute() {
            return Err("Refusing deletion outside absolute image cache root".into());
        }
        (self.remove)(path)
            .map_err(|e| format!("Cannot delete image cache entry {}: {e}", path.display()))
    }
}
