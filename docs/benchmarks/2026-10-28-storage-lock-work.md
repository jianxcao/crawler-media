# 共享锁与重复工作量：离线测量及复现

日期：2026-10-28。代码基线 `7e3bd653081c49220325e88b37997960966f5daa`，main `756444f76bef53f5fabed6d0ef53e9f280e005e9`。解释与修复提案见[复审](../audits/2026-10-28-storage-lock-review.md)。本页保存临时探针源码，避免只留下会消失的 `/tmp` 路径。

## 范围与方法

- macOS Apple Silicon，`rustc 1.99.1 (58cf8f9c3 2026-10-13)`。Store Probe/协议查询使用 release、rusqlite 0.37、bundled SQLite **3.50.2**；Router 直接链接本轮 workspace 的 debug API 产物。
- 每个数据点新建 tempfile 数据目录，关闭时自动清理。只用 fake Fetcher、MemoryDownloader、本地临时数据库；没有真实服务、网络、NAS、媒体读取或 STRM 扫描。
- 时间是本机单轮观测，不能作为部署 SLO。复核期间还运行了 workspace 测试，绝对耗时受 CPU 竞争影响；协议 VM/sort 计数和持锁注入用于确认结构问题。
- SQL/VM 随文件数增长的曲线与旧现场 491 个 STRM 不是同一种分布；不把一个 Media 的稠密 fixture 套成现场总耗时，也不宣称已经完成所有 API 的容量验收。

## A. 单文件 Probe 发布

fixture：一个 TV Media，N 个文件，每文件一个独立 `UnitKey`；先每 100 个文件调用公开 `insert_scanned_files`，初始化不计入 Probe 时间。先量一次发布，再对 N 个文件各发布一次。每次均断言成功，最终 episode_count 保留 N。这里直接测 Store，不持 ApiState Mutex；生产调用在该锁内执行同样的归约。

| N | seed 无统计 ms | 单次 Probe 无统计 ms | N 次 Probe 无统计 ms | seed 有统计 ms | 单次 Probe 有统计 ms | N 次 Probe 有统计 ms |
|---:|---:|---:|---:|---:|---:|---:|
| 100 | 16.164 | 3.639 | 201.309 | 7.443 | 1.440 | 61.681 |
| 200 | 20.518 | 7.267 | 1,431.473 | 20.188 | 1.658 | 176.822 |
| 400 | 76.506 | 28.426 | 11,484.707 | 76.066 | 2.277 | 576.588 |

无统计的 `sqlite_stat1` 不存在；有统计分支在装载后显式 ANALYZE。200→400 时累计约八倍；源码的逐单元相关查询加逐文件全量归约解释了该曲线。ANALYZE 改善计划，仍未消除每个文件重复归约全 Media。11.48 秒是 400 次调用的累计时间，不是一次持锁时长。临时原始输出为 `/tmp/crawler-store-review-result-latest.txt`。

## B. 协议 IsPlayed 的完整 count + page

fixture：一个 TV Media，一个文件覆盖 E 集；每集一条 played=true 断言。公开 `protocol_item_page_with_stats` 返回一个 Series、total=1，limit=10。测量包括该方法的所有语句，均为 10 statements。全新数据库，**未执行 ANALYZE**。

| E | VM steps | sorts | 首轮 ms | 复核轮 ms |
|---:|---:|---:|---:|---:|
| 250 | 2,015,920 | 502 | 7.53 | 6.026 |
| 500 | 8,031,420 | 1,002 | 28.29 | 22.016 |
| 1,000 | 32,062,420 | 2,002 | 101.65 | 109.673 |
| 2,000 | 128,124,420 | 4,002 | 415.45 | 340.761 |

两轮 VM/sort 完全一致；每翻倍约四倍 VM。此处是协议筛选谓词，不能以已修的 `series_scalar` 展示统计证明其成本已修。断言通过 SQL 直接装载临时 fixture，以排除逐集业务写入耗时；生产查询本身使用公开 Store 方法。该 fixture 不证明 played 写入后的用户摘要维护正确，后续修复应另外验证状态语义与子集覆盖。

## C. 当前 Router 的应用锁与读池

独立 OS 线程持 `state.store().lock()` 600ms；current_thread runtime 上同时启动 50ms timer，然后真实 Router oneshot。不用 runtime timer 释放锁。fixture 默认 TV Library 为空，detail/episodes 指向不存在的 Media，404 仍须完成当前读取流程；这证明拒绝路径也要等锁，未声称测过有文件详情的完整响应。

| 请求 | HTTP | 请求 ms | timer 唤醒 ms |
|---|---:|---:|---:|
| health | 200 | 9.58 | 52.61 |
| detail | 404 | 613.35 | 52.85 |
| libraries | 200 | 620.11 | 52.22 |
| episodes | 404 | 610.14 | 52.11 |
| items | 200 | 611.27 | 52.20 |

另取生产 reader 池的两个 lease，facets **0.99ms 返回 503**；释放两个 lease 后相同请求 200。此项不制造大查询，只确认容量用满时立即拒绝，尚无等空闲连接的队列。

阶段探针用已有 `with_library_read_stage_barrier`，只在 prepare 或 validate 前启动相同的独立持锁线程。items 返回 200、空页，reader 可用：

| 注入点 | HTTP ms | prepare 锁等待 ms | SQL ms | validate 锁等待 ms | timer ms |
|---|---:|---:|---:|---:|---:|
| prepare | 632.49 | 605.050 | 2.410 | 0.001 | 52.71 |
| validate | 629.72 | 0.0002 | 0.210 | 605.058 | 52.77 |

因此 offload 已保护异步执行器；读取开始前和返回前仍会排在共享 Store 锁后。这个实验没有实际 SQLite 写事务，不能作为 WAL 与 writer 并行已通过的证明；后续方案要增加真实写事务尚未提交时读取旧快照的行为测试。

最后一轮原始 Router/阶段输出：

```text
Baseline: 7e3bd653081c49220325e88b37997960966f5daa
Built API rlib: target/debug/deps/libapi-7672cccbc5d1358b.rlib (Oct 28 22:38, 238918736 bytes)
Original source: /tmp/storage_read_router_probe.rs
Recompiled executable: /tmp/storage_read_router_current_probe

case=health writer_hold_ms=600 status=200 OK request_ms=9.58 independent_timer_due_ms=50 observed_timer_ms=52.61
case=detail writer_hold_ms=600 status=404 Not Found request_ms=613.35 independent_timer_due_ms=50 observed_timer_ms=52.85
case=libraries writer_hold_ms=600 status=200 OK request_ms=620.11 independent_timer_due_ms=50 observed_timer_ms=52.22
case=episodes writer_hold_ms=600 status=404 Not Found request_ms=610.14 independent_timer_due_ms=50 observed_timer_ms=52.11
case=items writer_hold_ms=600 status=200 OK request_ms=611.27 independent_timer_due_ms=50 observed_timer_ms=52.20
case=production_2_pool_full status=503 Service Unavailable elapsed_ms=0.99
case=production_2_pool_released status=200 OK

Phase source: /tmp/storage_read_phase_current_probe.rs
Executable: /tmp/storage_read_phase_current_probe

held_only_at=prepare status=200 OK request_ms=632.49 independent_timer_ms=52.71 metrics=QueryMetricsSnapshot { request_id: 1, auth_lock_wait_ms: 0.0, auth_ms: 0.48925, credential_binding_ms: 0.0, queue_wait_ms: 0.21791700000000003, prepare_lock_wait_ms: 605.05, prepare_lock_held_ms: 0.7365419999999999, sql_ms: 2.410167, cold_ms: 0.0, validation_lock_wait_ms: 0.001166, validation_lock_held_ms: 0.29500000000000004, elapsed_ms: 620.265542, retry_count: 0, returned: Some(0), total: Some(0), vm_steps: Some(164), sort_operations: Some(0), fullscan_steps: Some(1), statements: Some(13), sqlite_profile_ms: Some(0.0), pool_rejections: 0, timeouts: 0, timeout_stage: None, remaining_budget_ms: None, worker_stop_delay_ms: None, pending_phases: [] }
held_only_at=validate status=200 OK request_ms=629.72 independent_timer_ms=52.77 metrics=QueryMetricsSnapshot { request_id: 2, auth_lock_wait_ms: 0.0, auth_ms: 0.086583, credential_binding_ms: 0.0, queue_wait_ms: 0.038834, prepare_lock_wait_ms: 0.00020800000000000001, prepare_lock_held_ms: 0.11945900000000001, sql_ms: 0.21000000000000002, cold_ms: 0.0, validation_lock_wait_ms: 605.057541, validation_lock_held_ms: 0.611708, elapsed_ms: 613.817416, retry_count: 0, returned: Some(0), total: Some(0), vm_steps: Some(164), sort_operations: Some(0), fullscan_steps: Some(1), statements: Some(13), sqlite_profile_ms: Some(0.0), pool_rejections: 0, timeouts: 0, timeout_stage: None, remaining_budget_ms: None, worker_stop_delay_ms: None, pending_phases: [] }
```

## D. Web 详情实际 adapter 的请求数量

加载当前 `web/lib/api/libraries.ts` 与 `library-file-pages.ts`，注入每页 50 文件的 fake HTTP。`getLibraryItemDetail` resolve 前，50/1,000/10,000 文件分别请求 1/20/200 次，最大同时在途请求均为 1。只证明完整收集与串行关系，不将 fake 的 1ms 延迟作为真实网络性能测量。此项不测试组件的定时器；重叠轮询风险来自组件缺少单在途与取消约束的源码核对。

将 D 源码保存到 `/tmp/storage-review-web-file-page-count.cjs`，在已安装当前 Web 依赖的 checkout 执行：

```sh
node /tmp/storage-review-web-file-page-count.cjs /Users/jianxcao/work/fun/crawler-media/web
```

## 重复构建方式

将下方 A、B 源码分别保存到一个**仓库外**临时 Cargo package 的 `src/bin/probe.rs`、`src/bin/played.rs`。以下 manifest 与测量直接依赖相同；将绝对仓库路径改为实际 checkout，核对 HEAD 与 SQLite 版本。依赖的精确解析以该 checkout 的 Cargo.lock 为参照；升级依赖后视为新测量，不能比较输出而不记录版本。

```toml
[package]
name = "storage-lock-review-probes"
version = "0.0.0"
edition = "2024"
[dependencies]
store = { path = "/Users/jianxcao/work/fun/crawler-media/crates/store" }
domain = { path = "/Users/jianxcao/work/fun/crawler-media/crates/domain" }
rusqlite = { version = "0.37", features = ["bundled"] }
tempfile = "3"
```

```sh
cargo run --release --manifest-path /tmp/storage-lock-review-probes/Cargo.toml --bin probe
cargo run --release --manifest-path /tmp/storage-lock-review-probes/Cargo.toml --bin played
```

Router 与阶段源码分别保存为 `/tmp/storage_read_router_probe.rs`、`/tmp/storage_read_phase_current_probe.rs`。本轮先由 workspace 测试构建当前代码，再使用下列同一依赖图的显式产物。hash 是本机本轮构建标识；其他 checkout 应从 cargo verbose/fingerprint 的匹配依赖图替换，不按 mtime 盲取其他 feature 图。运行产物只访问临时目录。

```sh
#!/bin/zsh
# Run after cargo test --workspace has compiled the reviewed HEAD. All paths below
# are from the matching dependency graph for API 7672cccbc5d1358b, not older 831b0f.
cd /Users/jianxcao/work/fun/crawler-media
rustc --edition=2024 /tmp/storage_read_router_probe.rs -L dependency=target/debug/deps --extern api=target/debug/deps/libapi-7672cccbc5d1358b.rlib --extern domain=target/debug/deps/libdomain-57f9db14c54a381b.rlib --extern axum=target/debug/deps/libaxum-c2c08d43676c17fa.rlib --extern downloader=target/debug/deps/libdownloader-2a6e4fa528d36cd8.rlib --extern indexer=target/debug/deps/libindexer-10cdc7f8ed294979.rlib --extern tower=target/debug/deps/libtower-419ace2a198dfcd0.rlib --extern tokio=target/debug/deps/libtokio-094711ed25271ae1.rlib --extern tempfile=target/debug/deps/libtempfile-00652038cc6f3a9c.rlib -o /tmp/storage_read_router_current_probe
rustc --edition=2024 /tmp/storage_read_phase_current_probe.rs -L dependency=target/debug/deps --extern api=target/debug/deps/libapi-7672cccbc5d1358b.rlib --extern domain=target/debug/deps/libdomain-57f9db14c54a381b.rlib --extern axum=target/debug/deps/libaxum-c2c08d43676c17fa.rlib --extern downloader=target/debug/deps/libdownloader-2a6e4fa528d36cd8.rlib --extern indexer=target/debug/deps/libindexer-10cdc7f8ed294979.rlib --extern tower=target/debug/deps/libtower-419ace2a198dfcd0.rlib --extern tokio=target/debug/deps/libtokio-094711ed25271ae1.rlib --extern tempfile=target/debug/deps/libtempfile-00652038cc6f3a9c.rlib --extern media_server=target/debug/deps/libmedia_server-a7f41f9aec934e30.rlib -o /tmp/storage_read_phase_current_probe
/tmp/storage_read_router_current_probe
/tmp/storage_read_phase_current_probe
```

## A 源码：公开 Probe 发布

```rust
use domain::{
    Confidence, FileVersion, LedgerId, Media, MediaId, MediaKind, QualitySource, UnitKey,
};
use std::time::Instant;
use store::library_write::probe::{Observed, ProbePublication};
use store::{ScannedLedger, Store};
fn main() {
    println!("SQLite {}", rusqlite::version());
    for analyzed in [false, true] {
        for n in [100, 200, 400] {
            let temp = tempfile::tempdir().unwrap();
            let s = Store::open(temp.path()).unwrap();
            let mid = MediaId::new();
            s.insert_media(&Media {
                id: mid,
                kind: MediaKind::Tv,
                title: "Review".into(),
                year: None,
                original_title: None,
                aliases: vec![],
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
            let lib = s
                .create_library(MediaKind::Tv, "Review", &["/review"], "everyone", true, &[])
                .unwrap();
            let rows: Vec<_> = (0..n)
                .map(|i| ScannedLedger {
                    file_id: LedgerId::new(),
                    media_id: mid,
                    path: format!("/review/e{i:05}.strm"),
                    units: vec![UnitKey::new(1, i)],
                    resolution: None,
                    codec: None,
                    hdr: None,
                    quality_source: QualitySource::Release,
                    confidence: Confidence::High,
                    filter_score: None,
                    source_version: "v1".into(),
                    source_digest: None,
                    size_bytes: None,
                    created_ns: None,
                    modified_ns: None,
                    artwork: Default::default(),
                    observations: vec![],
                })
                .collect();
            let start = Instant::now();
            for batch in rows.chunks(100) {
                s.insert_scanned_files(batch, 1000).unwrap();
            }
            let seed_ms = start.elapsed().as_secs_f64() * 1000.0;
            let c = rusqlite::Connection::open(temp.path().join("library.db")).unwrap();
            if analyzed {
                c.execute_batch("ANALYZE").unwrap();
            }
            let has_stats: i64 = c
                .query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE name='sqlite_stat1'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let expected = FileVersion {
                revision: 1,
                source_version: "v1".into(),
            };
            let p = ProbePublication {
                expected: &expected,
                tracks_version: 1,
                resolution: Some("1080p"),
                codec: Some("h264"),
                hdr: Observed::Value(None),
                duration_ms: Some(120000),
                details: None,
            };
            let start = Instant::now();
            assert!(s.publish_probe_result(rows[0].file_id, &p, 1001).unwrap());
            let one_ms = start.elapsed().as_secs_f64() * 1000.0;
            let start = Instant::now();
            for row in &rows {
                assert!(s.publish_probe_result(row.file_id, &p, 1002).unwrap());
            }
            let all_ms = start.elapsed().as_secs_f64() * 1000.0;
            let count = s
                .get_library_item(lib.id.parse().unwrap(), mid)
                .unwrap()
                .unwrap()
                .distinct_episode_count;
            println!(
                "analyzed={analyzed} n={n} stats_table={has_stats} seed_ms={seed_ms:.3} one_probe_ms={one_ms:.3} all_n_probes_ms={all_ms:.3} retained_episodes={count}"
            );
        }
    }
}
```

## B 源码：公开协议查询

```rust
use store::Store;

use domain::{
    Media, MediaId, MediaKind, ProtocolItemQuery, ProtocolItemType, ProtocolParent, UnitKey, UserId,
};
use std::time::Instant;
fn main() {
    for size in [250usize, 500, 1000, 2000] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let root = dir.path().join("tv");
        std::fs::create_dir_all(&root).unwrap();
        let lib = store
            .create_library(
                MediaKind::Tv,
                "TV",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let media = MediaId::new();
        let user = UserId::new();
        store
            .insert_user(&domain::User {
                id: user,
                login: "probe".into(),
                enabled: true,
                role: domain::UserRole::Member,
            })
            .unwrap();
        store
            .insert_media(&Media {
                id: media,
                kind: MediaKind::Tv,
                title: "Many episodes".into(),
                original_title: None,
                aliases: vec![],
                year: None,
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        let units = (1..=size)
            .map(|e| UnitKey::new(1, e as i32))
            .collect::<Vec<_>>();
        store
            .insert_scanned_files(
                &[store::ScannedLedger {
                    file_id: domain::LedgerId::new(),
                    media_id: media,
                    path: root.join("pack.mkv").display().to_string(),
                    units: units.clone(),
                    resolution: None,
                    codec: None,
                    hdr: None,
                    quality_source: domain::QualitySource::Release,
                    confidence: domain::Confidence::High,
                    filter_score: None,
                    source_version: "v1".into(),
                    source_digest: None,
                    size_bytes: None,
                    created_ns: None,
                    modified_ns: None,
                    artwork: Default::default(),
                    observations: vec![],
                }],
                1000,
            )
            .unwrap();
        let mut db = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
        let tx = db.transaction().unwrap();
        for e in 1..=size {
            tx.execute("INSERT INTO playback_units(user_id,media_id,season,episode,played,has_played_assertion,played_changed_at_ms) VALUES(?1,?2,1,?3,1,1,2)",rusqlite::params![user.to_string(),media.to_string(),e as i64]).unwrap();
        }
        tx.commit().unwrap();
        let q = ProtocolItemQuery {
            user_id: user,
            parent: ProtocolParent::Library(lib.id.parse().unwrap()),
            series_id: None,
            season: None,
            person_ids: vec![],
            studio_ids: vec![],
            search_term: None,
            genres: vec![],
            genre_ids: vec![],
            filters: vec![],
            is_favorite: None,
            is_played: Some(true),
            include_types: vec![ProtocolItemType::Series],
            recursive: true,
            is_folder: None,
            sort: vec![],
            offset: 0,
            limit: 10,
        };
        let t = Instant::now();
        let (p, s) = store.protocol_item_page_with_stats(&q).unwrap();
        println!(
            "units={size} rows={} total={} elapsed_ms={:.3} vm_steps={} sorts={} statements={}",
            p.items.len(),
            p.total,
            t.elapsed().as_secs_f64() * 1000.,
            s.vm_steps,
            s.sort_operations,
            s.statements
        );
    }
}
```

## C 源码：Router 与池容量

```rust
use api::{ApiState, Store, router};
use axum::{body::Body, http::Request};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use tower::ServiceExt;
struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}
fn main() {
    for which in ["health", "detail", "libraries", "episodes", "items"] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let library = store
            .default_library(domain::MediaKind::Tv)
            .unwrap()
            .unwrap()
            .id;
        let media = domain::MediaId::new();
        let state = ApiState::new(
            store,
            "management-secret".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
            dir.path().join("library"),
        )
        .unwrap();
        let handle = state.store();
        let app = router(state);
        let (tx, rx) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            let _guard = handle.lock();
            tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(600));
        });
        rx.recv().unwrap();
        let url = match which {
            "health" => "/api/v1/health".to_string(),
            "detail" => format!("/api/v1/libraries/{library}/items/{media}"),
            "libraries" => "/api/v1/libraries".to_string(),
            "episodes" => format!("/api/v1/libraries/{library}/items/{media}/episodes"),
            _ => format!("/api/v1/libraries/{library}/items"),
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
 let started=Instant::now();
 let timer=tokio::spawn(async move {tokio::time::sleep(Duration::from_millis(50)).await; started.elapsed().as_secs_f64()*1000.});
 tokio::task::yield_now().await;
 let response=app.oneshot(Request::get(url).header("Authorization","Bearer management-secret").body(Body::empty()).unwrap()).await.unwrap();
 let request_ms=started.elapsed().as_secs_f64()*1000.;
 let timer_ms=timer.await.unwrap();
 println!("case={which} writer_hold_ms=600 status={} request_ms={request_ms:.2} independent_timer_due_ms=50 observed_timer_ms={timer_ms:.2}",response.status());
 });
        writer.join().unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let library = store
        .default_library(domain::MediaKind::Tv)
        .unwrap()
        .unwrap()
        .id;
    let state = ApiState::new(
        store,
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap();
    let pool = state.library_read_pool_weak().upgrade().unwrap();
    let a = pool.try_checkout().unwrap();
    let b = pool.try_checkout().unwrap();
    let app = router(state);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let t = Instant::now();
        let response = app
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/libraries/{library}/facets"))
                    .header("Authorization", "Bearer management-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        println!(
            "case=production_2_pool_full status={} elapsed_ms={:.2}",
            response.status(),
            t.elapsed().as_secs_f64() * 1000.
        );
        drop(a);
        drop(b);
        let response = app
            .oneshot(
                Request::get(format!("/api/v1/libraries/{library}/facets"))
                    .header("Authorization", "Bearer management-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        println!(
            "case=production_2_pool_released status={}",
            response.status()
        );
    });
}
```

## C 补充源码：prepare/validate 分阶段

```rust
use api::{ApiState, Store, router};
use axum::{body::Body, http::Request};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media_server::{provider::QueryTiming, query_metrics::QueryMetrics};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};
use tower::ServiceExt;
struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}
fn main() {
    for stage in ["prepare", "validate"] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let lib = store
            .default_library(domain::MediaKind::Tv)
            .unwrap()
            .unwrap()
            .id;
        let state = ApiState::new(
            store,
            "management-secret".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
            dir.path().join("library"),
        )
        .unwrap();
        let (start_tx, start_rx) = mpsc::channel();
        let (locked_tx, locked_rx) = mpsc::channel();
        let locked_rx = Mutex::new(locked_rx);
        let store = state.store();
        let holder = std::thread::spawn(move || {
            start_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            let _guard = store.lock();
            locked_tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(600));
        });
        let first = AtomicBool::new(true);
        let state = state.with_library_read_stage_barrier(Arc::new(move |name| {
            if name == stage && first.swap(false, Ordering::SeqCst) {
                start_tx.send(()).unwrap();
                locked_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap();
            }
        }));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async{
let timing=QueryTiming::start();let metrics=QueryMetrics::new(timing,true);let mut req=Request::get(format!("/api/v1/libraries/{lib}/items")).header("Authorization","Bearer management-secret").body(Body::empty()).unwrap();req.extensions_mut().insert(timing);req.extensions_mut().insert(metrics.clone());
let t=Instant::now();let timer=tokio::spawn(async move{tokio::time::sleep(Duration::from_millis(50)).await;t.elapsed().as_secs_f64()*1000.});tokio::task::yield_now().await;
let response=router(state).oneshot(req).await.unwrap();let elapsed=t.elapsed().as_secs_f64()*1000.;let timer_ms=timer.await.unwrap();println!("held_only_at={stage} status={} request_ms={elapsed:.2} independent_timer_ms={timer_ms:.2} metrics={:?}",response.status(),metrics.snapshot());});
        holder.join().unwrap();
    }
}
```

## D 源码：生产 Web adapter 的离线请求计数

```javascript
// Offline probe: load the current production adapters, supply 50-file fake HTTP pages,
// and count all requests issued before getLibraryItemDetail resolves. No network/DB IO.
const fs = require('node:fs');
const path = require('node:path');
const { createRequire } = require('node:module');
const webRoot = path.resolve(process.argv[2] || '.');
const repoRequire = createRequire(path.join(webRoot, 'package.json'));
const ts = repoRequire('typescript');

function load(relative, requireMock) {
  const source = fs.readFileSync(path.join(webRoot, relative), 'utf8');
  const js = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const module = { exports: {} };
  new Function('require', 'module', 'exports', js)(requireMock, module, module.exports);
  return module.exports;
}

const collector = load('lib/library-file-pages.ts', () => ({}));

async function measure(fileCount) {
  let calls = 0;
  let active = 0;
  let peak = 0;
  const page = (start) => ({
    files: Array.from({ length: Math.min(50, fileCount - start) }, (_, index) => ({
      file_id: `f${start + index}`, presence: 'present', publication: 'published', units: [],
    })),
    next_cursor: start + 50 < fileCount ? String(start + 50) : null,
  });
  const api = load('lib/api/libraries.ts', (dependency) => {
    if (dependency === '@/lib/http') return {
      resolveRequestUrl: (value) => value,
      request: async (requestPath) => {
        calls += 1;
        active += 1;
        peak = Math.max(peak, active);
        // Artificial delay only establishes async sequencing. It measures no production latency.
        await new Promise((resolve) => setTimeout(resolve, 1));
        active -= 1;
        if (requestPath.endsWith('/items/media')) {
          const first = page(0);
          return { ok: true, data: {
            media_item_id: 'media', kind: 'tv', files: first.files,
            next_file_cursor: first.next_cursor,
          } };
        }
        const start = Number(new URL(requestPath, 'http://fixture').searchParams.get('after'));
        return { ok: true, data: page(start) };
      },
    };
    if (dependency.includes('library-file-pages')) return collector;
    return {};
  });
  const detail = await api.getLibraryItemDetail('library', 'media');
  return {
    files: fileCount,
    returned: detail.files.length,
    http_requests_before_detail_resolves: calls,
    peak_parallel_requests: peak,
  };
}

(async () => {
  for (const count of [50, 1000, 10000]) {
    process.stdout.write(`${JSON.stringify(await measure(count))}\n`);
  }
})().catch((error) => { console.error(error); process.exitCode = 1; });
```
