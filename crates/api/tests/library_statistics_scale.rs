//! A5 Library 统计：库表聚合与可见刷新状态（API-03）。
//!
//! 统计由 [`store::Store::library_roster_stats`] 一次聚合 `library_items` /
//! `library_files` 得出，不再依赖已删除的浏览投影：
//!
//! - 全局账本读取不随库数重复；
//! - 请求从不逐文件 `stat`（体积来自发布时写入的 `library_files.size_bytes`）；
//! - 体积只来自已发布文件事实（`.strm` 指针记 0）；
//! - 计数按 `library_id` 归属，改根不会复用或串用别的库的行。
//!
//! 注入的 [`CountingFiles`] 统计浏览文件 seam 的调用次数：统计走库表，任何一次
//! `/libraries` 请求都不得触发媒体文件 IO。

#[path = "support/mod.rs"]
mod support;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use api::browse_file_read::BrowseFileRead;
use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, Media, MediaId,
    MediaKind, QualitySource, UnitKey,
};
use serde_json::Value;
use store::library_write::{FilePublication as StorePublication, WriteScope};
use support::library_presence::PresenceFixture;

/// 注入 seam 里每个媒体文件的固定体积。
const FAKE_SIZE: u64 = 1024 * 1024;

/// 计数文件读取 seam：统计不得触发任何媒体文件 IO。
#[derive(Default)]
struct CountingFiles {
    metadata: AtomicUsize,
    read_nfo: AtomicUsize,
    is_file: AtomicUsize,
}

impl CountingFiles {
    fn total(&self) -> usize {
        self.metadata.load(Ordering::SeqCst)
            + self.read_nfo.load(Ordering::SeqCst)
            + self.is_file.load(Ordering::SeqCst)
    }

    fn reset(&self) {
        self.metadata.store(0, Ordering::SeqCst);
        self.read_nfo.store(0, Ordering::SeqCst);
        self.is_file.store(0, Ordering::SeqCst);
    }
}

impl BrowseFileRead for CountingFiles {
    fn read_nfo(&self, path: &Path) -> Option<library::NfoMeta> {
        self.read_nfo.fetch_add(1, Ordering::SeqCst);
        library::read_nfo(path)
    }

    fn metadata(&self, path: &Path) -> std::io::Result<std::fs::Metadata> {
        self.metadata.fetch_add(1, Ordering::SeqCst);
        std::fs::metadata(path)
    }

    fn is_file(&self, path: &Path) -> bool {
        self.is_file.fetch_add(1, Ordering::SeqCst);
        path.is_file()
    }
}

/// 一个 Library 的期望统计（由 fixture 自己算出，不读被测实现）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Expected {
    item_count: u64,
    file_count: u64,
    total_size_bytes: u64,
    strm_file_count: u64,
}

/// 一次 `/libraries` 请求观察到的文件/DB 工作量。
#[derive(Clone, Copy, Debug)]
struct Work {
    /// 无 WHERE 的根账本扫描步数（`list_file_coverage`）。
    ledger_scans: usize,
    /// 媒体文件 IO 次数（注入 seam）。
    media_stats: usize,
}

struct StatsFixture {
    fixture: PresenceFixture,
    files: Arc<CountingFiles>,
    expected: HashMap<String, Expected>,
}

fn empty_fixture() -> StatsFixture {
    let fixture = PresenceFixture::new();
    let files = Arc::new(CountingFiles::default());
    let state = fixture.state.clone().with_browse_file_read(files.clone());
    StatsFixture {
        fixture: PresenceFixture { state, ..fixture },
        files,
        expected: HashMap::new(),
    }
}

/// 库的唯一根 id（fixture 的库只有一个根）。
fn root_id_of(store: &store::Store, library_id: &str) -> String {
    store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|root| root.library_id.as_deref() == Some(library_id))
        .map(|root| root.id)
        .expect("library must own a root")
}

/// 发布一个文件事实：体积显式写入 `library_files`，统计只读库表、不 stat 文件。
fn publish_file(
    store: &store::Store,
    library: &store::Library,
    root_id: &str,
    media_id: MediaId,
    file_id: LedgerId,
    version: FileVersion,
    path: &Path,
    size_bytes: u64,
) {
    let file = LibraryFile {
        file_id,
        library_id: library.id.parse().unwrap(),
        root_id: root_id.to_string(),
        media_id,
        kind: library.kind,
        path: path.to_path_buf(),
        version,
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(size_bytes),
        stat_known: true,
        created_ns: None,
        modified_ns: None,
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: Some("1080p".into()),
        codec: Some("H264".into()),
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: None,
        tracks_version: 0,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    };
    let scope = WriteScope {
        library_id: library.id.parse().unwrap(),
        config_revision: 0,
    };
    store
        .publish_files(
            &scope,
            &[StorePublication {
                file: &file,
                units: &[UnitKey::WHOLE],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1000,
        )
        .unwrap();
}

fn new_file_version() -> FileVersion {
    FileVersion {
        revision: 1,
        source_version: format!("seed-{}", LedgerId::new()),
    }
}

fn insert_media(store: &store::Store, kind: MediaKind, title: &str) -> MediaId {
    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
            kind,
            title: title.to_string(),
            original_title: None,
            aliases: Vec::new(),
            year: Some(2000),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    media_id
}

/// 在 `root` 下种 `media_count` 部 Movie、每部 `files_per_media` 个文件；第
/// `strm_every` 个（1-based）写成 `.strm` 指针（`0` = 全部写成真实文件）。
fn seed_rows(
    store: &store::Store,
    library: &store::Library,
    root: &Path,
    tag: &str,
    kind: MediaKind,
    media_count: usize,
    files_per_media: usize,
    strm_every: usize,
) -> Expected {
    let root_id = root_id_of(store, &library.id);
    let mut expected = Expected {
        item_count: media_count as u64,
        file_count: 0,
        total_size_bytes: 0,
        strm_file_count: 0,
    };
    for index in 0..media_count {
        let title = format!("{tag}-{index:05}");
        let media_id = insert_media(store, kind, &title);
        for file in 0..files_per_media {
            let ordinal = index * files_per_media + file + 1;
            let strm = strm_every > 0 && ordinal % strm_every == 0;
            let name = if strm {
                format!("{title}.{file}.strm")
            } else {
                format!("{title}.{file}.mkv")
            };
            // `.strm` 指针文件只有几十字节 URL 文本，物理体积记 0。
            let size_bytes = if strm { 0 } else { FAKE_SIZE };
            publish_file(
                store,
                library,
                &root_id,
                media_id,
                LedgerId::new(),
                new_file_version(),
                &root.join(name),
                size_bytes,
            );
            expected.file_count += 1;
            if strm {
                expected.strm_file_count += 1;
            } else {
                expected.total_size_bytes += size_bytes;
            }
        }
    }
    expected
}

/// 建一个新建库：`name`、`kind`、单个 `root`（目录会建出来）。
fn new_library(
    store: &store::Store,
    dir: &Path,
    name: &str,
    kind: MediaKind,
    access_mode: &str,
    admin_visible: bool,
) -> (String, PathBuf) {
    let root = dir.join(name.replace(' ', "-").to_lowercase());
    std::fs::create_dir_all(&root).unwrap();
    let root_str = root.display().to_string();
    let library = store
        .create_library(
            kind,
            name,
            &[root_str.as_str()],
            access_mode,
            admin_visible,
            &[],
        )
        .unwrap();
    (library.id, root)
}

fn library_of(store: &store::Store, id: &str) -> store::Library {
    store.get_library(id).unwrap().expect("library must exist")
}

/// `libraries` 个 Movie 库，每库 `media_per_library` 部作品 × `files_per_media` 个文件。
fn seed(
    libraries: usize,
    media_per_library: usize,
    files_per_media: usize,
    strm_every: usize,
) -> StatsFixture {
    let mut seeded = empty_fixture();
    let mut expected = HashMap::new();
    {
        let store_arc = seeded.fixture.state.store();
        let guard = store_arc.lock();
        let primary = library_of(&guard, &seeded.fixture.library_id);
        expected.insert(
            primary.id.clone(),
            seed_rows(
                &guard,
                &primary,
                &seeded.fixture.root,
                "A",
                MediaKind::Movie,
                media_per_library,
                files_per_media,
                strm_every,
            ),
        );
        for index in 1..libraries {
            let (id, root) = new_library(
                &guard,
                seeded.fixture.dir.path(),
                &format!("Movies {index}"),
                MediaKind::Movie,
                "everyone",
                true,
            );
            let library = library_of(&guard, &id);
            expected.insert(
                id,
                seed_rows(
                    &guard,
                    &library,
                    &root,
                    &format!("B{index}"),
                    MediaKind::Movie,
                    media_per_library,
                    files_per_media,
                    strm_every,
                ),
            );
        }
    }
    seeded.expected = expected;
    seeded
}

impl StatsFixture {
    async fn list(&self) -> Value {
        let (status, body) = self
            .fixture
            .request("GET", "/api/v1/libraries", Some(&self.fixture.admin_token))
            .await;
        assert_eq!(status, 200, "{body}");
        body["data"].clone()
    }

    /// 重置计数器 → 一次 `/libraries` → 读数。
    async fn list_with_work(&self) -> (Value, Work) {
        self.fixture.state.store().lock().reset_root_ledger_scans();
        self.files.reset();
        let body = self.list().await;
        let work = Work {
            ledger_scans: self.fixture.state.store().lock().root_ledger_scans(),
            media_stats: self.files.total(),
        };
        eprintln!(
            "work: ledger_scans={} media_stats={}",
            work.ledger_scans, work.media_stats
        );
        (body, work)
    }

    fn stats(body: &Value, library_id: &str) -> Value {
        body.as_array()
            .expect("library list must be an array")
            .iter()
            .find(|library| library["id"] == library_id)
            .unwrap_or_else(|| panic!("library {library_id} must be listed"))["stats"]
            .clone()
    }

    /// 每库统计必须与 fixture 自己算出的期望逐字段一致。
    fn assert_stats(&self, body: &Value) {
        for (id, expected) in &self.expected {
            let stats = Self::stats(body, id);
            assert_eq!(
                stats["item_count"].as_u64(),
                Some(expected.item_count),
                "item_count {id}"
            );
            assert_eq!(
                stats["file_count"].as_u64(),
                Some(expected.file_count),
                "file_count {id}"
            );
            assert_eq!(
                stats["total_size_bytes"].as_u64(),
                Some(expected.total_size_bytes),
                "total_size_bytes {id}"
            );
            assert_eq!(
                stats["strm_file_count"].as_u64(),
                Some(expected.strm_file_count),
                "strm_file_count {id}"
            );
            // 统计每次请求都从库表现算，没有「过期投影」状态。
            assert_eq!(stats["stale"], false, "库表聚合必须是最新值 {id}");
        }
    }
}

/// 一次聚合：同样的账本总量（600 行）下，20 个库不得比 1 个库多扫全局账本。
#[tokio::test]
async fn library_list_does_not_repeat_global_reads_per_library() {
    let one = seed(1, 600, 1, 0);
    let many = seed(20, 30, 1, 0);
    let (one_body, one_work) = one.list_with_work().await;
    let (many_body, many_work) = many.list_with_work().await;
    one.assert_stats(&one_body);
    many.assert_stats(&many_body);
    assert_eq!(
        many_work.ledger_scans, one_work.ledger_scans,
        "20 个库不得比 1 个库多扫全局账本：1 库 {} 步、20 库 {} 步",
        one_work.ledger_scans, many_work.ledger_scans
    );
    assert_eq!(many_work.ledger_scans, 0, "统计不得扫描全局账本");
}

/// 统计不逐文件 stat：冷请求（首次）与热请求都零媒体文件 IO，且统计不变。
#[tokio::test]
async fn hot_statistics_do_not_stat_each_media_file() {
    let seeded = seed(1, 10, 4, 4);
    let library = seeded.fixture.library_id.clone();
    let (cold_body, cold_work) = seeded.list_with_work().await;
    assert_eq!(cold_work.media_stats, 0, "冷统计不得 stat 每个媒体文件");
    let cold = StatsFixture::stats(&cold_body, &library);
    assert_eq!(cold["item_count"].as_u64(), Some(10));
    assert_eq!(cold["file_count"].as_u64(), Some(40));
    assert_eq!(
        cold["total_size_bytes"].as_u64(),
        Some(30 * FAKE_SIZE),
        "体积必须来自发布事实，且 .strm 指针记 0：{cold}"
    );
    assert_eq!(cold["strm_file_count"].as_u64(), Some(10));

    // 两次请求之间文件没有任何变化：热请求同样零媒体文件 IO。
    let (hot_body, hot_work) = seeded.list_with_work().await;
    assert_eq!(hot_work.media_stats, 0, "热统计不得 stat 每个媒体文件");
    let hot = StatsFixture::stats(&hot_body, &library);
    assert_eq!(hot, cold, "无变化时两次请求的统计必须逐字段一致");
}

/// `.strm` 指针文件只有几十字节 URL 文本，不计入物理体积。
#[tokio::test]
async fn strm_pointer_files_do_not_add_physical_size() {
    let seeded = seed(1, 4, 5, 5);
    let library = seeded.fixture.library_id.clone();
    let body = seeded.list().await;
    let stats = StatsFixture::stats(&body, &library);
    assert_eq!(stats["file_count"].as_u64(), Some(20));
    assert_eq!(stats["strm_file_count"].as_u64(), Some(4));
    assert_eq!(stats["total_size_bytes"].as_u64(), Some(16 * FAKE_SIZE));
}

/// 同一部作品在多库各有拷贝：每库各自计入，不做全局去重后串库。
#[tokio::test]
async fn same_media_in_two_libraries_is_counted_per_library() {
    let seeded = empty_fixture();
    let first = seeded.fixture.library_id.clone();
    let second;
    {
        let store_arc = seeded.fixture.state.store();
        let guard = store_arc.lock();
        let (id, root) = new_library(
            &guard,
            seeded.fixture.dir.path(),
            "Second",
            MediaKind::Movie,
            "everyone",
            true,
        );
        second = id;
        let media_id = insert_media(&guard, MediaKind::Movie, "Shared");
        let first_library = library_of(&guard, &first);
        let second_library = library_of(&guard, &second);
        let first_root = root_id_of(&guard, &first);
        let second_root = root_id_of(&guard, &second);
        for (library, root_id, root_path) in [
            (&first_library, &first_root, &seeded.fixture.root),
            (&second_library, &second_root, &root),
        ] {
            publish_file(
                &guard,
                library,
                root_id,
                media_id,
                LedgerId::new(),
                new_file_version(),
                &root_path.join("Shared.mkv"),
                FAKE_SIZE,
            );
        }
    }
    let body = seeded.list().await;
    let first_stats = StatsFixture::stats(&body, &first);
    let second_stats = StatsFixture::stats(&body, &second);
    for (id, stats) in [(&first, &first_stats), (&second, &second_stats)] {
        assert_eq!(
            stats["item_count"].as_u64(),
            Some(1),
            "每库各自计入同一部作品 {id}"
        );
        assert_eq!(
            stats["file_count"].as_u64(),
            Some(1),
            "每库文件数必须独立 {id}"
        );
        assert_eq!(
            stats["total_size_bytes"].as_u64(),
            Some(FAKE_SIZE),
            "每库体积必须独立 {id}"
        );
    }
}

/// 隐藏 / 冻结（removal 中）的库不得把自己的行混进可见库统计；管理员管理范围仍
/// 能看到冻结库的真实计数。
#[tokio::test]
async fn hidden_and_blocked_libraries_do_not_leak_into_visible_stats() {
    let seeded = empty_fixture();
    let visible = seeded.fixture.library_id.clone();
    let hidden;
    let blocked;
    {
        let store_arc = seeded.fixture.state.store();
        let guard = store_arc.lock();
        let primary = library_of(&guard, &visible);
        seed_rows(
            &guard,
            &primary,
            &seeded.fixture.root,
            "V",
            MediaKind::Movie,
            2,
            1,
            0,
        );
        let (id, root) = new_library(
            &guard,
            seeded.fixture.dir.path(),
            "Hidden",
            MediaKind::Movie,
            "selected",
            false,
        );
        hidden = id;
        let library = library_of(&guard, &hidden);
        seed_rows(&guard, &library, &root, "H", MediaKind::Movie, 3, 1, 0);
        let (id, root) = new_library(
            &guard,
            seeded.fixture.dir.path(),
            "Blocked",
            MediaKind::Movie,
            "everyone",
            true,
        );
        blocked = id;
        let library = library_of(&guard, &blocked);
        seed_rows(&guard, &library, &root, "B", MediaKind::Movie, 4, 1, 0);
        guard.begin_library_removal(&blocked).unwrap();
    }
    let (status, body) = seeded
        .fixture
        .request(
            "GET",
            "/api/v1/libraries",
            Some(&seeded.fixture.member_token),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let listed: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|library| library["id"].as_str())
        .collect();
    assert!(
        !listed.contains(&hidden.as_str()),
        "非成员库不得出现在成员列表"
    );
    assert!(
        !listed.contains(&blocked.as_str()),
        "removal 中的库不得出现在成员列表"
    );
    assert_eq!(
        StatsFixture::stats(&body["data"], &visible)["item_count"].as_u64(),
        Some(2),
        "不可见库的行不得混进可见库统计"
    );
    let (status, admin_body) = seeded
        .fixture
        .request(
            "GET",
            "/api/v1/libraries?scope=admin",
            Some(&seeded.fixture.admin_token),
        )
        .await;
    assert_eq!(status, 200, "{admin_body}");
    assert_eq!(
        StatsFixture::stats(&admin_body["data"], &blocked)["item_count"].as_u64(),
        Some(4),
        "管理范围仍要给出冻结库的真实计数"
    );
}

/// 首次请求（从未浏览过）也不得返回假 0：计数与体积都来自已发布库表。
#[tokio::test]
async fn cold_statistics_are_not_fake_zero() {
    let seeded = empty_fixture();
    let library = seeded.fixture.library_id.clone();
    {
        let store_arc = seeded.fixture.state.store();
        let guard = store_arc.lock();
        let primary = library_of(&guard, &library);
        seed_rows(
            &guard,
            &primary,
            &seeded.fixture.root,
            "A",
            MediaKind::Movie,
            3,
            2,
            0,
        );
    }
    // 不先浏览：统计没有「未构建的投影」这一前置条件。
    let (status, body) = seeded
        .fixture
        .request(
            "GET",
            "/api/v1/libraries",
            Some(&seeded.fixture.admin_token),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let stats = StatsFixture::stats(&body["data"], &library);
    assert_eq!(
        stats["item_count"].as_u64(),
        Some(3),
        "作品数必须来自库表：{stats}"
    );
    assert_eq!(
        stats["file_count"].as_u64(),
        Some(6),
        "文件数必须来自库表：{stats}"
    );
    assert_eq!(
        stats["strm_file_count"].as_u64(),
        Some(0),
        "strm 文件数必须精确：{stats}"
    );
    assert_eq!(
        stats["total_size_bytes"].as_u64(),
        Some(6 * FAKE_SIZE),
        "体积必须来自发布事实：{stats}"
    );
    assert_eq!(stats["stale"], false, "库表聚合没有过期态：{stats}");
}

/// 改根（scope 变化）后统计仍按 `library_id` 归属，不复用也不丢失已有行。
#[tokio::test]
async fn library_scope_change_does_not_reuse_old_statistics() {
    let seeded = empty_fixture();
    let tv_other;
    let old_root;
    {
        let store_arc = seeded.fixture.state.store();
        let guard = store_arc.lock();
        let (id, root) = new_library(
            &guard,
            seeded.fixture.dir.path(),
            "TV Other",
            MediaKind::Tv,
            "everyone",
            true,
        );
        tv_other = id;
        let library = library_of(&guard, &tv_other);
        seed_rows(&guard, &library, &root, "T", MediaKind::Tv, 2, 1, 0);
        old_root = root;
    }
    let before = seeded.list().await;
    let other_before = StatsFixture::stats(&before, &tv_other);
    assert_eq!(other_before["file_count"].as_u64(), Some(2));
    assert_eq!(
        other_before["total_size_bytes"].as_u64(),
        Some(2 * FAKE_SIZE)
    );

    // 改范围：给同一个库再加一个空根（既有根保留，已发布行不动）。
    let extra = seeded.fixture.dir.path().join("tv-other-extra");
    std::fs::create_dir_all(&extra).unwrap();
    let old_str = old_root.display().to_string();
    let extra_str = extra.display().to_string();
    seeded
        .fixture
        .state
        .store()
        .lock()
        .update_library_full(
            &tv_other,
            None,
            Some(&[old_str.as_str(), extra_str.as_str()]),
            None,
            None,
            None,
        )
        .unwrap();

    let after = seeded.list().await;
    let other_after = StatsFixture::stats(&after, &tv_other);
    assert_eq!(
        other_after["item_count"].as_u64(),
        Some(2),
        "scope 变化后作品数仍必须来自库表：{other_after}"
    );
    assert_eq!(
        other_after["file_count"].as_u64(),
        Some(2),
        "scope 变化后文件数仍必须来自库表：{other_after}"
    );
    assert_eq!(
        other_after["strm_file_count"].as_u64(),
        Some(0),
        "scope 变化后 strm 文件数仍必须来自库表：{other_after}"
    );
    assert_eq!(
        other_after["total_size_bytes"].as_u64(),
        Some(2 * FAKE_SIZE),
        "同一 library_id 的已发布体积不得因加根而丢失：{other_after}"
    );
}

/// 同数量文件的 `.mkv` → `.strm` 变化后，物理体积必须按新发布事实归零。
#[tokio::test]
async fn same_count_strm_change_does_not_reuse_physical_bytes_as_fresh() {
    let seeded = empty_fixture();
    let library_id = seeded.fixture.library_id.clone();
    let store_arc = seeded.fixture.state.store();
    let (root_id, media_id, file_id, from, to) = {
        let guard = store_arc.lock();
        let library = library_of(&guard, &library_id);
        let root_id = root_id_of(&guard, &library_id);
        let media_id = insert_media(&guard, MediaKind::Movie, "A-00000");
        let file_id = LedgerId::new();
        let from = seeded.fixture.root.join("A-00000.0.mkv");
        publish_file(
            &guard,
            &library,
            &root_id,
            media_id,
            file_id,
            new_file_version(),
            &from,
            FAKE_SIZE,
        );
        (
            root_id,
            media_id,
            file_id,
            from,
            seeded.fixture.root.join("A-00000.0.strm"),
        )
    };
    let before = seeded.list().await;
    let old = StatsFixture::stats(&before, &library_id);
    assert_eq!(old["total_size_bytes"].as_u64(), Some(FAKE_SIZE));
    assert_eq!(old["stale"], false, "库表聚合必须是最新值");

    {
        let guard = store_arc.lock();
        assert!(
            guard
                .rename_file_path(&from.display().to_string(), &to.display().to_string())
                .unwrap()
        );
        // 扫描器观察到指针文件后按新体积重发布同一文件（同一 revision）。
        let version = guard
            .get_file_version(file_id)
            .unwrap()
            .expect("file version");
        let library = library_of(&guard, &library_id);
        publish_file(
            &guard, &library, &root_id, media_id, file_id, version, &to, 0,
        );
    }

    let after = seeded.list().await;
    let stats = StatsFixture::stats(&after, &library_id);
    assert_eq!(stats["file_count"].as_u64(), Some(1));
    assert_eq!(stats["strm_file_count"].as_u64(), Some(1));
    assert_eq!(
        stats["total_size_bytes"].as_u64(),
        Some(0),
        "同数量 .strm 替换后不得复用旧的物理体积：{stats}"
    );
}

/// The authenticated ordinary roster resolves the caller role once, independent of N.
#[tokio::test]
async fn ordinary_library_list_performs_one_role_lookup_for_many_libraries() {
    let seeded = seed(8, 0, 0, 0);
    {
        let store = seeded.fixture.state.store();
        store.lock().reset_role_lookups();
    }
    let (status, body) = seeded
        .fixture
        .request(
            "GET",
            "/api/v1/libraries",
            Some(&seeded.fixture.member_token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    assert_eq!(seeded.fixture.state.store().lock().role_lookups(), 1);
}
