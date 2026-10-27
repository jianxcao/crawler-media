//! Seed cache for the performance fixture.
//!
//! Seeding is ~50,000 write transactions, so the gate keeps a copy keyed on everything that
//! determines the database: the fixture version, the scale, the SQLite version and the
//! checked-in DDL. A layout change therefore never reuses an old seed.
//!
//! # One seeder at a time
//!
//! Several agents run this gate at once, so a cache miss must not turn into two processes
//! each spending ~50,000 writes on the same database: they starve each other on fsync and
//! neither finishes (observed: a realistic seed exceeded 40 minutes with a second seeder
//! running). A `<key>.lock` file decides the turn:
//!
//! * `create_new` wins the lock and owns the seed; it publishes under a staging directory and
//!   one atomic `rename`, then removes the lock (a `Drop` guard covers a panicking seed).
//! * Any other process waits, polling until the cache entry appears (it then copies that
//!   copy) or the lock file disappears (the seeder failed; try to acquire).
//! * A lock older than [`STALE_LOCK_SECS`] is treated as a crashed seeder and is stolen, so a
//!   waiter can never be blocked forever. `PERF_FIXTURE_CACHE=off` skips all of this and
//!   seeds privately, which is the escape hatch when the cache directory is unusable.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::fixture::{Fixture, SeededLibrary, Scale, FIXTURE_VERSION};

const ENV: &str = "PERF_FIXTURE_CACHE";
const DATABASES: [&str; 4] = ["app.db", "cache.db", "library.db", "subscribe.db"];
/// A lock this old belongs to a seeder that died; a waiter may steal it.
/// `PERF_FIXTURE_CACHE_STALE_SECS` overrides it (tests use a few seconds).
const STALE_LOCK_SECS: u64 = 45 * 60;
/// How often a waiter re-checks for the finished cache entry.
const POLL: Duration = Duration::from_secs(5);

/// Whose turn it is to produce this fixture.
///
/// `Ready` carries the whole fixture, which is much larger than the other variants. Boxing it
/// would only save stack space for a value that exists once per process, so the difference is
/// deliberate.
#[allow(clippy::large_enum_variant)]
pub enum CacheSlot {
    /// This process won the seed turn and must seed, then [`publish`] before dropping it.
    Owner(SeedGuard),
    /// Another process finished the seed while we waited.
    Ready(Fixture),
    /// No usable cache (disabled, unwritable, or a broken lock file): seed privately.
    Unavailable,
}

/// Holds the `<key>.lock` file for as long as the seed turn is ours.
pub struct SeedGuard {
    path: PathBuf,
}

impl Drop for SeedGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Decides whether this process seeds and publishes, or waits for another process's copy.
pub fn take_turn(scale: Scale) -> CacheSlot {
    let Some(dir) = cache_dir(scale) else {
        return CacheSlot::Unavailable;
    };
    if let Some(parent) = dir.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return CacheSlot::Unavailable;
    }
    let lock = dir.with_extension("lock");
    let mut waited = false;
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
        {
            Ok(_) => {
                let guard = SeedGuard { path: lock };
                // The seeder may have finished between our last check and this `create_new`;
                // its entry is already complete, so use it instead of seeding again.
                if is_ready(&dir)
                    && let Some(fixture) = lookup(scale)
                {
                    drop(guard);
                    return CacheSlot::Ready(fixture);
                }
                if waited {
                    println!("fixture: took the seed turn after waiting");
                }
                return CacheSlot::Owner(guard);
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                if lock_is_stale(&lock) {
                    eprintln!(
                        "fixture: stealing a stale seed lock at {} (older than {}s)",
                        lock.display(),
                        stale_lock().as_secs()
                    );
                    let _ = std::fs::remove_file(&lock);
                    continue;
                }
                if is_ready(&dir) {
                    return match lookup(scale) {
                        Some(fixture) => CacheSlot::Ready(fixture),
                        None => CacheSlot::Unavailable,
                    };
                }
                if !waited {
                    println!(
                        "fixture: another process is seeding {}; waiting instead of duplicating \
                         ~50,000 write transactions",
                        dir.display()
                    );
                    waited = true;
                }
                std::thread::sleep(POLL);
            }
            Err(error) => {
                eprintln!("fixture: not caching seeds ({error})");
                return CacheSlot::Unavailable;
            }
        }
    }
}

fn stale_lock() -> Duration {
    let secs = std::env::var("PERF_FIXTURE_CACHE_STALE_SECS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(STALE_LOCK_SECS);
    Duration::from_secs(secs)
}

fn lock_is_stale(lock: &Path) -> bool {
    let Ok(modified) = std::fs::metadata(lock).and_then(|meta| meta.modified()) else {
        return true;
    };
    SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age > stale_lock())
}

/// A complete cache entry: manifest plus every database file.
fn is_ready(dir: &Path) -> bool {
    dir.join("manifest.json").is_file() && DATABASES.iter().all(|name| dir.join(name).is_file())
}

/// The cache copy for this scale, or `None` when caching is off or has nothing usable.
pub fn lookup(scale: Scale) -> Option<Fixture> {
    let dir = cache_dir(scale)?;
    if !is_ready(&dir) {
        return None;
    }
    let manifest = std::fs::read_to_string(dir.join("manifest.json")).ok()?;
    let temp = tempfile::tempdir().unwrap();
    for name in DATABASES {
        for suffix in ["", "-wal"] {
            let from = dir.join(format!("{name}{suffix}"));
            if from.is_file() {
                std::fs::copy(&from, temp.path().join(format!("{name}{suffix}"))).ok()?;
            }
        }
    }
    let store = store::Store::open(temp.path()).unwrap();
    println!("fixture: seed cache hit at {}", dir.display());
    Some(from_manifest(temp, store, scale, &manifest))
}

/// Copies a freshly seeded fixture into the cache. Called before any measurement, because
/// the measured operations mutate their own copy (publication consumes its slots).
pub fn publish(fixture: &Fixture) {
    let Some(dir) = cache_dir(fixture.scale) else {
        return;
    };
    if let Some(parent) = dir.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let staging = match tempfile::tempdir_in(dir.parent().unwrap_or(Path::new("."))) {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("fixture: not caching seeds ({error})");
            return;
        }
    };
    for name in DATABASES {
        for suffix in ["", "-wal"] {
            let from = fixture.dir.path().join(format!("{name}{suffix}"));
            if from.is_file() {
                std::fs::copy(&from, staging.path().join(format!("{name}{suffix}"))).unwrap();
            }
        }
    }
    std::fs::write(staging.path().join("manifest.json"), manifest(fixture)).unwrap();
    // A previous seeder may have died mid-copy; its leftovers are not a usable entry and we
    // hold the lock, so clear them. Otherwise the single `rename` publishes the whole entry
    // atomically and never clobbers a copy another process already finished.
    if dir.exists() && !is_ready(&dir) {
        let _ = std::fs::remove_dir_all(&dir);
    }
    match std::fs::rename(staging.path(), &dir) {
        Ok(()) => println!("fixture: seeded, cached at {}", dir.display()),
        Err(error) => eprintln!(
            "fixture: seeded but could not publish the cache entry at {} ({error}); \
             keeping the existing one",
            dir.display()
        ),
    }
}

fn cache_dir(scale: Scale) -> Option<PathBuf> {
    let configured = std::env::var(ENV).unwrap_or_default();
    if configured == "off" {
        return None;
    }
    let root = if configured.is_empty() {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("target/perf-fixtures")
    } else {
        PathBuf::from(configured)
    };
    let key = format!(
        "v{FIXTURE_VERSION}-m{}-c{}-s{}-e{}-o{}-p{}-sql{}-ddl{}",
        scale.movies,
        scale.movie_copies,
        scale.shows,
        scale.episodes_per_show,
        scale.other,
        scale.publish_slots,
        rusqlite::version().replace('.', "_"),
        ddl_hash(),
    );
    Some(root.join(key))
}

/// Hash of the checked-in DDL, so a layout change never reuses an old seed.
fn ddl_hash() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/schema");
    let mut sources: Vec<PathBuf> = Vec::new();
    collect_sql(&root, &mut sources);
    sources.sort();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for path in sources {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        for byte in text.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}

fn collect_sql(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_sql(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "sql") {
            out.push(path);
        }
    }
}

fn manifest(fixture: &Fixture) -> String {
    let library = |seeded: &SeededLibrary| {
        serde_json::json!({
            "library_id": seeded.library_id.to_string(),
            "root_id": seeded.root_id,
            "root": seeded.root,
            "config_revision": seeded.config_revision,
        })
    };
    serde_json::json!({
        "fixture_version": FIXTURE_VERSION,
        "locale": fixture.locale,
        "user": fixture.user.to_string(),
        "token": fixture.token,
        "integrity_operation_id": fixture.integrity_operation_id,
        "movies": library(&fixture.movies),
        "shows": library(&fixture.shows),
    })
    .to_string()
}

fn from_manifest(
    dir: tempfile::TempDir,
    store: store::Store,
    scale: Scale,
    manifest: &str,
) -> Fixture {
    let raw: serde_json::Value = serde_json::from_str(manifest).unwrap();
    let seeded = |value: &serde_json::Value| SeededLibrary {
        library_id: value["library_id"].as_str().unwrap().parse().unwrap(),
        root_id: value["root_id"].as_str().unwrap().to_string(),
        root: value["root"].as_str().unwrap().to_string(),
        config_revision: value["config_revision"].as_i64().unwrap(),
    };
    Fixture::from_parts(
        dir,
        store,
        scale,
        raw["locale"].as_str().unwrap().to_string(),
        raw["user"].as_str().unwrap().parse().unwrap(),
        raw["token"].as_str().unwrap().to_string(),
        seeded(&raw["movies"]),
        seeded(&raw["shows"]),
        raw["integrity_operation_id"].as_str().unwrap().to_string(),
    )
}
