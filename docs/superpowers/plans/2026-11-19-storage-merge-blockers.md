# storage-redesign 六项审查问题实现方案

> 执行时使用 `superpowers:executing-plans`，按各任务的复现、实现、验证顺序推进。复选框用于记录实际完成情况。

## 目标与基线

修复本轮相对 `origin/main` 审查发现的六项问题，使 Library 删除、重配置、根目录登记、语言保存、分集选择和浏览器恢复播放具备可验证的完成条件。

| 项目 | 基线 |
| --- | --- |
| 工作目录 | `/Users/jianxcao/work/fun/crawler-media` |
| 当前分支 | `feat/storage-redesign` |
| 审查代码 | `d7423e19ebf6d739abf5baea31ee32f3445fe2bc` |
| 比较基点 | `origin/main`，`756444f76bef53f5fabed6d0ef53e9f280e005e9` |
| 分支提交范围 | 基点之后 17 个提交 |
| 本文状态 | 六项修复与追加阶段校验已实现；浏览器、静态检查及修复后定向验证通过，验证明细见末尾 |

设计沿用 SQLite 持久化操作、每库单 writer、读池、Library 工作登记和现有播放器状态机。根目录准入复用已持久化的重配置计划，播放恢复复用会话签发及代次校验。主要技术为 Rust、rusqlite、axum、Tokio、React、TypeScript 和 Node 测试运行器。

执行前阅读相关约束。

- [AGENTS.md](/Users/jianxcao/work/fun/crawler-media/AGENTS.md)
- [CONTEXT.md](/Users/jianxcao/work/fun/crawler-media/CONTEXT.md) 中 Library、Unidentified、Playback 和 Job 段落
- [ADR-0008](/Users/jianxcao/work/fun/crawler-media/docs/adr/0008-sqlite-system-of-record.md)
- [ADR-0012](/Users/jianxcao/work/fun/crawler-media/docs/adr/0012-library-removal.md)
- [Self API 约定](/Users/jianxcao/work/fun/crawler-media/docs/api-contracts/self.md)

## 共同约束

- 每项先在现有 public seam 写失败测试，保留失败输出，再实现并重跑。断言 HTTP 响应、持久化操作、文件归属、用户进度和实际播放请求。
- 文件系统路径观察在 writer 外完成。事务内完成最新状态校验及对应写入。数据库 writer、Registry Mutex 和 reader lease 均在同步作用域内释放。
- 接口读使用 `store_read` 或相应 Store reader。跨数据库的清理继续通过现有持久化完成状态协调。
- Library 删除成功后保留磁盘内容。仍由独立嵌套 Library 持有的 Media、favorites 和播放进度应可读。
- 媒体信息 worker 默认 1、远程读取并发默认 1、远程读取间隔默认 500ms。两个并发配置范围均为 1～8，间隔范围为 0～60000ms。
- 测试依赖注入离线 Fetcher、Catalog 和 Probe。临时目录使用 `/tmp`，结束时释放任务和临时文件。
- 所有 build/test 命令经过 `scripts/with-build-lock.py`。普通 Cargo 使用 `-j4` 和 4 个测试线程。最终 workspace 使用独占两槽、`-j8`、8 个测试线程和 `--no-fail-fast`，执行一次。
- Rust 文件硬上限 800 行，生产和测试函数硬上限均为 120 行。新增代码按职责放入现有模块或文中列出的新文件。
- 每个任务完成定向验证后形成独立提交，最后汇总 crate 级与 workspace 级验证结果。提交限定当前分支及本任务文件。

## 问题、方案与顺序

| 任务 | 优先级 | 已观察到的行为 | 预期交付 |
| --- | --- | --- | --- |
| 1 | P1 | 250 个文件归属尚在迁移时删除父 Library，嵌套 Library 只留下 100 条记录 | 删除准入与迁移状态互斥，迁移完成后完整保留嵌套归属 |
| 2 | P1 | 16 个重建操作返回 202，只有 2 个成功，其余停在 frozen，retry 返回 409 | 已受理操作等待工作槽，真实中断持久化为可重试失败 |
| 3 | P1 | 根目录冲突先返回 202，随后 UNIQUE 失败并冻结 Library | 在登记事务内发现既有或待执行计划的冲突，返回 409 |
| 4 | P2 | 保存相同语言仍取消正在运行的 Library 工作 | 相同值保存具有幂等语义 |
| 5 | P2 | 待发布或缺失文件对应的分集被标记为 owned | 播放分集清单只使用 `Playable` 文件 |
| 6 | P2 | BFCache 恢复后仍使用已关闭的播放 SID | 缓存期间保存进度，恢复时取得受校验的新 SID |

推荐顺序为任务 1 → 2 → 3 → 4 → 5 → 6 → 集成验收。任务 1、3 会触及 Store 的操作准入，应顺序落地。任务 3 的失败计划修正复用任务 2 建立的 driver claim，避免旧 driver 收尾与修正请求竞态。任务 4 复用任务 2 的操作完成测试。任务 5、6 各自可独立验证。

## Task 1 · 删除前检查根目录归属迁移

### 文件与接口

| 文件 | 修改职责 |
| --- | --- |
| [library_deletion/connection.rs](/Users/jianxcao/work/fun/crawler-media/crates/store/src/library_deletion/connection.rs) | 在 `begin_library_removal` 的事务内增加准入校验 |
| [library_operations/integrity_guard.rs](/Users/jianxcao/work/fun/crawler-media/crates/store/src/library_operations/integrity_guard.rs) | 完善共享的 `ensure_no_root_reassignment` |
| [roots.rs](/Users/jianxcao/work/fun/crawler-media/crates/store/src/roots.rs) | 供内部调用者复用 `component_range` |
| [root_reassign_batched.rs](/Users/jianxcao/work/fun/crawler-media/crates/store/tests/root_reassign_batched.rs) | 覆盖 250 个文件的迁移、删除与重开 Store |
| [root_unidentified.rs](/Users/jianxcao/work/fun/crawler-media/crates/store/tests/root_unidentified.rs) | 覆盖仅存在 Unidentified 的待迁移范围 |
| [library_removal_cleanup.rs](/Users/jianxcao/work/fun/crawler-media/crates/store/tests/library_removal_cleanup.rs) | 验证 Media 和用户事实保留 |
| [storage_api_contract/library_patch.rs](/Users/jianxcao/work/fun/crawler-media/crates/api/tests/storage_api_contract/library_patch.rs) | 使用 `PresenceFixture` 验证 DELETE 拒绝后的可见状态 |
| [ADR-0012](/Users/jianxcao/work/fun/crawler-media/docs/adr/0012-library-removal.md) | 补充删除受理前的迁移冲突语义 |

复用内部接口。

```rust
pub(crate) fn ensure_no_root_reassignment(
    tx: &rusqlite::Connection,
    library: domain::LibraryId,
) -> Result<(), crate::StoreError>;
```

HTTP 契约为 `409 library.protected`。拒绝发生在删除登记前，Library 继续可见、revision 保持原值，现有工作继续执行。迁移完成后重新发起 DELETE，进入 ADR-0012 的隐藏、立即取消、清理流程。

### 测试与实施步骤

- [ ] 在 `root_reassign_batched.rs` 复用现有 `media`、`owned_by`，加入下面的完整主回归。`set_root_reassign_batch_budget` 在创建返回后立即恢复。

```rust
#[test]
fn removal_waits_for_nested_root_reassignment() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let data = tmp.path().join("data");
    let parent = std::fs::canonicalize(tmp.path()).unwrap().join("parent");
    let nested = parent.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let store = Store::open(&data).unwrap();
    let parent_library = store
        .create_library(
            MediaKind::Movie, "Parent", &[parent.to_str().unwrap()],
            "everyone", true, &[],
        )
        .unwrap();
    let mut ids = Vec::new();
    for index in 0..250 {
        let path = nested.join(format!("{index:03}.mkv"));
        std::fs::write(&path, b"media").unwrap();
        let id = media(&store);
        publish_fixture_file(
            &store,
            &FixtureFile {
                media_id: id,
                path: path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        ids.push(id);
    }
    set_root_reassign_batch_budget(Some(1));
    let created = store.create_library(
        MediaKind::Movie, "Nested", &[nested.to_str().unwrap()],
        "everyone", true, &[],
    );
    set_root_reassign_batch_budget(None);
    let nested_library = created.unwrap();
    assert_eq!(owned_by(&store, &parent_library.id), 150);
    assert_eq!(owned_by(&store, &nested_library.id), 100);

    let rejected = store.begin_library_removal(&parent_library.id);
    assert!(matches!(rejected, Err(store::StoreError::Protected(_))));
    assert!(store.get_library(&parent_library.id).unwrap().is_some());
    assert_eq!(owned_by(&store, &parent_library.id), 150);
    drop(store);

    let store = Store::open(&data).unwrap();
    store.drain_root_reassignments().unwrap();
    let removal = store.begin_library_removal(&parent_library.id).unwrap();
    store.set_removal_state(&removal.operation_id, "cleaning", None).unwrap();
    let done = store.clean_library_removal(&removal.operation_id).unwrap();
    assert_eq!(done.state, "succeeded");
    assert_eq!(owned_by(&store, &nested_library.id), 250);
    for id in ids {
        assert!(store.get_media(id).unwrap().is_some());
    }
    for index in 0..250 {
        assert_eq!(
            std::fs::read(nested.join(format!("{index:03}.mkv"))).unwrap(),
            b"media",
        );
    }
}
```

- [ ] 运行主回归并确认当前代码在删除应被拒绝的断言处失败。

```sh
python3 scripts/with-build-lock.py cargo test -j4 -p store \
  --test root_reassign_batched removal_waits_for_nested_root_reassignment \
  -- --test-threads=4
```

- [ ] 在 `begin_library_removal` 返回既有删除 receipt 的分支之后、首次登记之前，加入共享 guard。既有 receipt 的重试继续复用同一删除操作。

```rust
let library = id.parse::<domain::LibraryId>()?;
crate::library_operations::integrity_guard::ensure_no_operation(&tx, library)?;
crate::library_operations::integrity_guard::ensure_no_root_reassignment(
    &tx,
    library,
)?;
```

- [ ] 扩展 guard 的候选集合，使用待处理状态和索引范围查询。

  1. 遍历 `root_reassignment_state` 中 `state <> 'succeeded'` 的范围，覆盖 queued、running、blocked、failed。
  2. 待删除 Library 是目标 Library 时直接拒绝。
  3. 源文件候选同时匹配 `library_id`、`kind` 和路径范围。文件的 MediaKind 语义与 `reassign_root_batch` 一致。
  4. Unidentified 候选匹配 `library_id` 和路径范围。当前 Unidentified 迁移按所有 enabled roots 解析归属，因此此分支覆盖跨 MediaKind 的嵌套情况。
  5. 路径匹配使用 exact key 与 `component_range(root)` 的半开区间。将现有 helper 的可见性缩小到 `pub(crate)` 所需范围。分别执行两个 `EXISTS`，复用路径索引。
  6. 校验和删除登记使用同一个 Library writer 事务。失败直接返回 `StoreError::Protected`。

```sql
SELECT EXISTS(
    SELECT 1 FROM library_files
    WHERE library_id = ?1 AND kind = ?2 AND path_key = ?3
) OR EXISTS(
    SELECT 1 FROM library_files
    WHERE library_id = ?1 AND kind = ?2
      AND path_key >= ?4 AND path_key < ?5
);
```

这里的参数依次为源 Library、迁移 MediaKind、root key、区间下界和上界。Unidentified 查询采用相同的两段范围和自身的归属字段。

- [ ] 加入以下行为测试，每种场景均通过 Store 或 HTTP 的正常写入口构造。

| 场景 | 断言 |
| --- | --- |
| 删除迁移目标 Library | 409，待迁移状态继续可恢复 |
| 删除范围外的 Library | 正常受理并完成 |
| 只有 Unidentified backlog | 删除源 Library 被拒绝，迁移完成后记录归属正确 |
| `/root/nested` 与 `/root/nested-other` | 后者的独立操作可正常受理 |
| 根目录 `/` | 子路径候选可以命中 |
| 不同 MediaKind 的文件 | 按实际文件迁移范围判定 |
| 同一 Media 同时在父库和嵌套库有文件 | 删除父库后，嵌套库文件、favorites、已看状态、进度可读 |
| guard 拒绝时持有 Library work lease | `control().is_cancelled()` 为 false |
| 已有删除 receipt 的重复 DELETE | 返回原操作并完成幂等清理 |

- [ ] 运行上述三个 Store target、`storage_api_contract` 和原有 `library_removal_topology`、`removal_explicit_ownership`。补充 ADR 后提交本任务。

## Task 3 · 原子检查已登记与待执行的根目录占用

### 文件与接口

新增内部模块 `/Users/jianxcao/work/fun/crawler-media/crates/store/src/library_operations/root_conflicts.rs`，集中实现已有根及待执行 roots 计划的占用检查。

| 文件 | 修改职责 |
| --- | --- |
| [library_operations/mod.rs](/Users/jianxcao/work/fun/crawler-media/crates/store/src/library_operations/mod.rs) | 声明内部模块 |
| [library_operations/connection.rs](/Users/jianxcao/work/fun/crawler-media/crates/store/src/library_operations/connection.rs) | 登记重配置前调用检查 |
| [library_operations/roots.rs](/Users/jianxcao/work/fun/crawler-media/crates/store/src/library_operations/roots.rs) | 应用 roots 批次时复查相同约束 |
| [libraries/root_config.rs](/Users/jianxcao/work/fun/crawler-media/crates/store/src/libraries/root_config.rs) | `replace_roots` 调用共享检查 |
| [libraries/connection.rs](/Users/jianxcao/work/fun/crawler-media/crates/store/src/libraries/connection.rs) | 保证创建、配置写入的事务边界 |
| [roots/connection.rs](/Users/jianxcao/work/fun/crawler-media/crates/store/src/roots/connection.rs) | `insert_library_root_prepared` 使用同一检查 |
| [library_config.rs](/Users/jianxcao/work/fun/crawler-media/crates/api/src/http/library_config.rs) | 创建、PATCH 的冲突响应 |
| [library_operations.rs](/Users/jianxcao/work/fun/crawler-media/crates/api/src/http/library_operations.rs) | 重配置入口的冲突响应 |
| [library_reconfiguration_roots.rs](/Users/jianxcao/work/fun/crawler-media/crates/store/tests/library_reconfiguration_roots.rs) | Store 级准入、并发、恢复测试 |
| [storage_api_contract/library_patch.rs](/Users/jianxcao/work/fun/crawler-media/crates/api/tests/storage_api_contract/library_patch.rs) | HTTP 事务原子性测试 |

新增内部接口。

```rust
pub(crate) fn ensure_root_keys_available<'a>(
    tx: &rusqlite::Connection,
    library_id: &str,
    keys: impl IntoIterator<Item = &'a str>,
) -> Result<(), crate::StoreError>;

#[derive(Debug, thiserror::Error)]
pub enum RootOperationRetryError {
    #[error("operation does not own the requested frozen revision")]
    NotOwner,
    #[error("root operation has committed batches")]
    NotReplannable,
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl Store {
    pub fn replace_failed_roots_operation(
        &self,
        operation_id: &str,
        expected_revision: i64,
        roots: &PreparedLibraryRoots,
        now_ms: i64,
    ) -> Result<LibraryOperationView, RootOperationRetryError>;
}
```

选择 `StoreError::Conflict` 表达其他 Library 对相同 canonical root key 的占用，HTTP 返回 `409 library.root_conflict`。请求内部重复 root 继续按无效参数返回 400。根目录的祖先、后代关系按现有最长匹配规则处理。

### 测试与实施步骤

- [ ] 在 `library_reconfiguration_roots.rs` 添加主回归。

```rust
#[test]
fn conflicting_root_is_rejected_before_freeze() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let a_root = temp.path().join("a");
    let b_root = temp.path().join("b");
    std::fs::create_dir_all(&a_root).unwrap();
    std::fs::create_dir_all(&b_root).unwrap();
    let a = store.create_library(
        MediaKind::Movie, "A", &[a_root.to_str().unwrap()],
        "everyone", true, &[],
    ).unwrap();
    let b = store.create_library(
        MediaKind::Movie, "B", &[b_root.to_str().unwrap()],
        "everyone", true, &[],
    ).unwrap();
    let revision = store.library_config_revision(&a.id).unwrap();
    let request = LibraryReconfiguration {
        operation_id: "root-conflict".into(),
        library_id: a.id.parse().unwrap(),
        expected_revision: revision,
        root_paths: Some(vec![a_root, b_root]),
        display_locale: None,
    };
    let prepared = request.prepare().unwrap();
    let result = store.begin_library_reconfiguration_prepared(&prepared, 1_000);
    assert!(matches!(result, Err(store::StoreError::Conflict(_))));
    assert_eq!(store.library_lifecycle_state(&a.id).unwrap(), "ready");
    assert_eq!(store.library_config_revision(&a.id).unwrap(), revision);
    assert!(store.active_library_operation(&a.id).unwrap().is_none());
    assert_eq!(store.get_library(&b.id).unwrap().unwrap().root_paths, b.root_paths);
}
```

- [ ] 运行测试，确认当前实现在预期冲突处错误地成功登记。

```sh
python3 scripts/with-build-lock.py cargo test -j4 -p store \
  --test library_reconfiguration_roots conflicting_root_is_rejected_before_freeze \
  -- --test-threads=4
```

- [ ] 实现共享占用检查。

  1. `prepare()`、`PreparedLibraryRoots::observe()` 继续在 writer 外产生 canonical key。
  2. 事务内用 `canonical_recorded_key` 对齐已有 alias 记录的比较语义。
  3. 检查 `library_roots.path_key` 对应的 owner，允许请求 Library 自己持有的根。该 Library 的既有根拥有已提交归属，其他操作中的冲突请求应由其自身的失败恢复处理。
  4. 读取其他 Library 的 unfinished roots 操作，使用现有 `unfinished_operation_predicate()`，解码 `ReconfigurationPlan.roots` 并比较 key。retryable failed 操作仍持有其请求根目录。
  5. 计划解码失败返回具体 Store 错误并回滚本次请求。
  6. 批量处理本次 roots 和活跃 plans，使工作量随根目录及活跃操作数量增长。

```sql
SELECT library_id
FROM library_roots
WHERE path_key = ?1 AND library_id <> ?2
LIMIT 1;
```

待执行计划的读取复用当前操作谓词，选取 `kind='roots'`、其他 Library 和 `plan_json`。成功登记的 operation row 即构成持久化占用。重启时根据同一 plan 判定。

- [ ] 将检查放入全部根目录写入口的写事务。检查顺序为删除屏障、操作所有权与 revision、根目录冲突、覆盖校验、配置和操作写入、commit。对允许同一 owner 的内部 apply 过程使用相同 helper。

- [ ] 为当前已持久化的冲突 roots 操作提供修正入口。扩展现有 `POST /libraries/{id}/operations/{operation}/retry`，允许 failed roots 操作携带修正后的 `root_paths` 和当前冻结的 `expected_revision`。

```json
{
  "expected_revision": 1,
  "root_paths": ["/media/movies"]
}
```

  - 空 body 继续表示执行同一计划。带 body 的 roots 修正请求先在 writer 外调用 `PreparedLibraryRoots::observe`。
  - `replace_failed_roots_operation` 在同一事务核对 operation 属于请求 Library、kind 为 roots、state 为 failed、Library revision 等于请求值且等于操作原 `expected_revision + 1`。
  - 只有 `cursor_json IS NULL` 的操作可以修正。该条件证明首个批次尚未提交，适用于根目录 UNIQUE 冲突发生在首批的当前复现场景。
  - 校验新 roots 的占用及对本 Library 已记录文件的覆盖，然后替换 `plan_json.roots`，保留 operation ID、其他计划字段和冻结 revision。state 继续为 failed，随后由 retry driver 推进。
  - 已提交批次的请求返回 `409 library.operation_not_replannable`，普通 retry 继续使用持久化 cursor。
  - 错误 revision 返回 `409 library.operation_not_owner`，修正后的 root 冲突返回 `409 library.root_conflict`。所有拒绝均保留原计划。
  - `RootOperationRetryError` 在 `library_operations` 模块公开，HTTP 按枚举变体映射错误码，`StoreError::Conflict` 继续表示 root 占用冲突。
  - 任务 2 接入 driver claim 后，body 解析及锁外观察先完成，取得 claim 后再执行上述 Store 事务，确保计划修正与实际 retry 准入一致。

这项恢复能力作用于 failed roots 操作的正常生命周期，使用现有持久化字段。API 契约记录修正范围及限制。

- [ ] 在 HTTP 错误映射中添加以下分支，并为创建 Library、PATCH、显式 reconfigure 三条实际入口验证响应。

```rust
Err(StorageOperationError::Store(StoreError::Conflict(message))) => err(
    StatusCode::CONFLICT,
    "library.root_conflict",
    &message,
),
```

- [ ] 补充并发和失败回归。

| 场景 | 断言 |
| --- | --- |
| A、B 在 writer 外准备同一个空闲 root，用 barrier 同时提交 | 恰好一个成功登记，另一个返回 Conflict 并保持 ready |
| A 已受理，尚未执行首批，B 创建 Library 使用该 root | B 创建事务回滚 |
| A 已受理，B 调用 `insert_library_root` | 返回 Conflict |
| A 的 roots 操作进入 retryable failed | B 仍无法登记该 root，A 可以 retry |
| A 已成功释放旧 root | 其他 Library 可正常登记该旧 root |
| 两个 symlink 指向同一目录 | 按 canonical key 发现冲突 |
| 同一 owner 保留自己的 root | 成功登记 |
| 独立嵌套 roots | 按现有归属规则成功登记 |
| PATCH 同时改名称、可见性、探测开关及冲突 root | 全部配置读回原值，既有 work lease 的取消标记为 false |
| 重开 Store 后登记竞争 root | 仍识别持久化计划的占用 |
| 首批 UNIQUE 冲突留下的 failed roots 操作 | 使用合法 roots 修正后，原 operation 成功、Library 恢复 ready |
| 有已提交 cursor 的 failed roots 操作 | 修正 body 返回明确 409，原 plan 和 cursor 可读 |

- [ ] 重跑 `library_reconfiguration_roots`、`root_write_ownership`、`terminal_operation_admission`、`storage_api_contract`。更新 Self API 的 409 契约并提交。

## Task 2 · 保证已受理重配置获得执行机会

### 文件与接口

| 文件 | 修改职责 |
| --- | --- |
| [blocking_budget.rs](/Users/jianxcao/work/fun/crawler-media/crates/api/src/blocking_budget.rs) | 增加等待容量的 reservation |
| [blocking_budget/tests.rs](/Users/jianxcao/work/fun/crawler-media/crates/api/src/blocking_budget/tests.rs) | 验证等待、取消和 worker 持有 permit |
| [storage_operations/driver.rs](/Users/jianxcao/work/fun/crawler-media/crates/api/src/storage_operations/driver.rs) | driver step、失败落库、finalize |
| [storage_operations.rs](/Users/jianxcao/work/fun/crawler-media/crates/api/src/storage_operations.rs) | 普通启动和重启恢复的 driver 准入 |
| [library_lifecycle.rs](/Users/jianxcao/work/fun/crawler-media/crates/api/src/library_lifecycle.rs) | driver claim 与释放顺序 |
| [library_operations.rs](/Users/jianxcao/work/fun/crawler-media/crates/api/src/http/library_operations.rs) | retry 与旧 driver 收尾的并发准入 |
| [library_operations/mod.rs](/Users/jianxcao/work/fun/crawler-media/crates/store/src/library_operations/mod.rs) | 公开最小失败状态写接口 |
| [library_operations/connection.rs](/Users/jianxcao/work/fun/crawler-media/crates/store/src/library_operations/connection.rs) | 在 writer 内进行所有权 CAS |
| [metadata_language_operation.rs](/Users/jianxcao/work/fun/crawler-media/crates/api/tests/metadata_language_operation.rs) | 16 库 fan-out、容量占满、超时及 retry |
| [storage_operations_runtime.rs](/Users/jianxcao/work/fun/crawler-media/crates/api/tests/storage_operations_runtime.rs) | 启动恢复、批次推进与错误终态 |

新增两个接口。

```rust
impl BlockingBudget {
    pub async fn reserve_until_available(
        &self,
    ) -> Result<Reservation, BlockingError>;
}

impl Store {
    pub fn fail_library_operation_if_owned(
        &self,
        operation_id: &str,
        message: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError>;
}
```

`true` 表示本次把当前 owner 的非终态操作转为 failed。`false` 表示该操作已终止、已失败、已消失或所有权已变化。调用者随后读取现状，保留已有的终态和更具体的错误。

### 测试与实施步骤

- [ ] 复用 `metadata_language_operation.rs` 的 `stage`、`json_request`、`body_json`、`wait_for_operation`，加入以下 fan-out 回归。

```rust
#[tokio::test]
async fn accepted_language_operations_wait_for_capacity() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let state = stage(&temp);
    for index in 0..14 {
        let root = temp.path().join(format!("movie-{index}"));
        std::fs::create_dir_all(&root).unwrap();
        state.store().create_library(
            domain::MediaKind::Movie, &format!("Movie {index}"),
            &[root.to_str().unwrap()], "everyone", true, &[],
        ).unwrap();
    }
    let reservations = [
        state.fs_work.try_reserve().unwrap(),
        state.fs_work.try_reserve().unwrap(),
    ];
    let app = router(state.clone());
    let response = app.clone().oneshot(json_request(
        "PATCH",
        "/api/v1/settings/metadata-language",
        serde_json::json!({"expected_revision":0,"language":"en-US"}),
    )).await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = body_json(response).await;
    let operations = body["data"]["operations"].as_array().unwrap();
    assert_eq!(operations.len(), 16);
    // Give the dispatched tasks a scheduling turn while both slots remain owned.
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    drop(reservations);
    for operation in operations {
        let library = operation["library_id"].as_str().unwrap();
        let id = operation["operation_id"].as_str().unwrap();
        let poll = format!("/api/v1/libraries/{library}/operations/{id}");
        wait_for_operation(&app, &poll).await;
        assert_eq!(state.store().library_lifecycle_state(library).unwrap(), "ready");
    }
}
```

增加同一 fan-out 的无预留槽版本，覆盖多个 driver 自然竞争 2 个执行槽的原始场景。等待类原语的确定性时序由下一步的 budget 测试覆盖。

- [ ] 运行回归，确认旧实现会留下无法完成的 frozen 操作。

```sh
python3 scripts/with-build-lock.py cargo test -j4 -p api \
  --test metadata_language_operation accepted_language_operations_wait_for_capacity \
  -- --test-threads=4
```

- [ ] 为新 reservation 写行为测试。先占满容量，启动等待 future 并验证处于 pending。释放一个 reservation 后等待者取得容量。取消等待 future 后其他等待者能继续取得容量。blocking worker 正在运行时释放外层 future，permit 仍由实际 worker 持有。

- [ ] 实现窄范围的等待 API。

```rust
pub async fn reserve_until_available(&self) -> Result<Reservation, BlockingError> {
    self.permits
        .clone()
        .acquire_owned()
        .await
        .map(|permit| Reservation { permit })
        .map_err(|error| BlockingError::Worker(error.to_string()))
}
```

- [ ] `driver_step` 和 `finalize` 先等待 reservation，再将它移动进 `Reservation::run_to_completion`。排队期间仅持有 operation ID、Library ID 和 `ApiState`。实际 blocking worker 内再申请 reconfiguration work lease。

```rust
let reservation = state
    .fs_work
    .reserve_until_available()
    .await
    .map_err(|error| error.to_string())?;
reservation
    .run_to_completion(move || {
        let _lease = owned.library_work.acquire_for_reconfiguration(
            &owned.store,
            &library_id,
            &operation_id,
        )?;
        let now_ms = owned.playback_clock.now_ms();
        owned.store.advance_library_operation(&operation_id, now_ms, BATCH_MEDIA)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
```

该代码展示 advance 分支的执行顺序，`owned`、`library_id`、`operation_id` 沿用 `driver_step` 现有 owned captures，`now_ms` 在 worker 内从 `owned.playback_clock` 取得。完整分支在 advance 后读取并返回 `LibraryOperationView`。每批完成后释放 reservation，下一批重新进入容量队列。

- [ ] 对 driver 错误进行持久化收尾。

  - `DRAIN_TIMEOUT` 继续为 30 秒，超时写 failed 并保留 cursor。
  - worker 失败、游标停止推进和实际 Store 错误带 operation、Library、阶段及原始错误记录日志。
  - Store 的 batch 已写 failed 时保留已有错误。
  - `fail_library_operation_if_owned` 在 Library writer 内核对 `expected_revision + 1`、`lifecycle_state='reindexing'` 和当前 unfinished owner。仅修改非终态操作的 state、error、updated_at。
  - failure 写入的 CAS 与成功完成竞争时，succeeded 保持可观察终态。
  - 确认失败状态已落库或 owner 已终止后，释放 Registry 的 block 和 driver claim，并唤醒 root reassignment worker。
  - 数据库暂时 Busy 时保留 driver claim，通过现有有界 writer 准入再次完成失败落库。数据库不可写时记录落库失败的独立错误，保留持久化 cursor 和启动恢复路径，验收报告明确该基础设施故障。

```sql
UPDATE library_operations
SET state = 'failed', error = ?2, updated_at_ms = ?3
WHERE operation_id = ?1
  AND state IN ('planned', 'frozen', 'draining', 'applying', 'checking')
  AND EXISTS(
      SELECT 1 FROM libraries l
      WHERE l.library_id = library_operations.library_id
        AND l.lifecycle_state = 'reindexing'
        AND l.config_revision = library_operations.expected_revision + 1
  );
```

执行时将当前 unfinished owner 的检查合入同一事务，并通过 `changed_rows` 返回 CAS 结果。`planned` 覆盖 Integrity 的起始状态。

- [ ] 消除 retry 与旧 driver 收尾的竞争窗口。将已有 claim、block、spawn 拆成清晰的准入顺序。

  1. 普通启动先取得 `claim_driver`，再 block 和启动。提取内部 `spawn_claimed_driver(state, operation_id, library_id)` 执行已持有 claim 的派发，调用者负责生命周期校验。
  2. retry 先验证 User 可执行写入，再尝试取得同一个 driver claim。已有 driver 时返回 `409 library.operation_running`。
  3. retry 取得 claim 后重新读取持久化 operation，验证仍为 failed、仍是本 Library 当前 owner。验证失败释放 claim，返回对应的已有冲突响应。
  4. 验证成功后执行 block，再调用 `spawn_claimed_driver`，返回 202。任何派发失败都释放本次 claim，并保留原失败状态。
  5. 旧 driver 的 finalize 先释放其 block，最后释放 claim。新 retry 获得 claim 时，旧收尾已完成。
  6. 在测试中暂停旧 driver 的收尾阶段，发送 retry 并观察明确 409。收尾完成后重试返回 202，原 operation 最终成功。再覆盖 retry 已读到 failed、实际取得 claim 前旧操作已完成的情况，确保已有普通 work lease 继续运行。

- [ ] 增加恢复和错误行为测试。

| 场景 | 断言 |
| --- | --- |
| 原工作 lease 在取消后仍未 drop | 使用 Tokio paused time 推进 30 秒，GET operation 返回 failed 和明确 drain 错误 |
| 上述 lease 释放后 POST retry | 原 operation ID 继续，最终 ready |
| 同一 failed operation 的并发 retry | 一个请求取得执行机会，其余返回明确冲突，原 operation 和 cursor 一致 |
| 16 个持久化操作重开 Store 后启动恢复 | 容量为 2 时全部到达终态 |
| Store 批次主动返回错误 | failed、cursor 保留，retry 可继续 |
| 操作在异常收尾前已经 succeeded | CAS 返回 false，GET 仍显示 succeeded |
| 普通 HTTP 工作的 budget 已满 | 维持原有 fail-fast 响应契约 |

- [ ] 重跑 `metadata_language_operation`、`storage_operations_runtime`、`terminal_operation_admission` 和 budget 测试，更新 API 的失败/retry 说明并提交。

## Task 4 · 相同语言保存只返回当前状态

### 文件与接口

修改 [metadata_language.rs](/Users/jianxcao/work/fun/crawler-media/crates/api/src/http/metadata_language.rs)，扩展 [metadata_language_operation.rs](/Users/jianxcao/work/fun/crawler-media/crates/api/tests/metadata_language_operation.rs)。复用 `MetadataLanguageState.revision` 与请求的 `expected_revision` 判断本次是否登记了操作。

### 测试与实施步骤

- [ ] 在现有测试文件添加幂等保存回归。先通过 HTTP 修改语言，等待本轮全部 operation 完成，然后持有新的 Library lease。

```rust
#[tokio::test]
async fn unchanged_language_preserves_admitted_work() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let state = stage(&temp);
    let app = router(state.clone());
    let first = app.clone().oneshot(json_request(
        "PATCH", "/api/v1/settings/metadata-language",
        serde_json::json!({"expected_revision":0,"language":"en-US"}),
    )).await.unwrap();
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let changed = body_json(first).await;
    for operation in changed["data"]["operations"].as_array().unwrap() {
        let poll = format!(
            "/api/v1/libraries/{}/operations/{}",
            operation["library_id"].as_str().unwrap(),
            operation["operation_id"].as_str().unwrap(),
        );
        wait_for_operation(&app, &poll).await;
    }
    let completed = app.clone().oneshot(request(
        "GET", "/api/v1/settings/metadata-language",
    )).await.unwrap();
    let baseline = body_json(completed).await;
    let library = state.store().default_library(domain::MediaKind::Movie)
        .unwrap().unwrap();
    let work = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Ok(work) = state.acquire_library_work(&library.id) {
                break work;
            }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    let response = app.clone().oneshot(json_request(
        "PATCH", "/api/v1/settings/metadata-language",
        serde_json::json!({"expected_revision":1,"language":" en-US "}),
    )).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let current = body_json(response).await;
    assert_eq!(current["data"]["revision"], 1);
    assert_eq!(current["data"]["operations"], baseline["data"]["operations"]);
    assert!(!work.control().is_cancelled());
}
```

- [ ] 运行回归并确认旧代码会将 lease 标记为 cancelled。

```sh
python3 scripts/with-build-lock.py cargo test -j4 -p api \
  --test metadata_language_operation unchanged_language_preserves_admitted_work \
  -- --test-threads=4
```

- [ ] 将副作用限定到本次 revision 发生变化的成功结果。

```rust
if let Ok(value) = &result
    && value.revision != expected_revision
{
    for operation in &value.operations {
        let library = operation.library_id.to_string();
        crate::storage_operations::spawn_driver(
            &work_state,
            &operation.operation_id,
            &library,
        );
    }
}
```

这里的 `spawn_driver` 使用任务 2 完成的 claim → block → spawn 流程。状态相同的请求直接返回当前 `MetadataLanguageState`。

- [ ] 覆盖同值的三种当前状态。历史操作 succeeded 时工作继续，当前操作 pending 时保持原 driver，当前操作 failed 时返回既有失败状态。显式 retry 入口负责后续重试。
- [ ] 验证 stale `expected_revision` 仍返回 `409 settings.conflict`，真实变更有操作时返回 202，同值保存返回 200。
- [ ] 重跑完整 `metadata_language_operation`，在 Self API 中记录幂等语义并提交。

## Task 5 · 播放分集查询统一采用 Playable

### 文件与接口

| 文件 | 修改职责 |
| --- | --- |
| [ledger.rs](/Users/jianxcao/work/fun/crawler-media/crates/store/src/ledger.rs) | season 查询增加显式 scope |
| [ledger/connection.rs](/Users/jianxcao/work/fun/crawler-media/crates/store/src/ledger/connection.rs) | 复用范围谓词并保留按季索引查询 |
| [playback/episode_listing.rs](/Users/jianxcao/work/fun/crawler-media/crates/api/src/http/playback/episode_listing.rs) | 单季和全部季分支均传 `Playable` |
| [library_chapters/refresh.rs](/Users/jianxcao/work/fun/crawler-media/crates/api/src/http/library_chapters/refresh.rs) | 显式选择既有业务所需的 `AllRecorded` |
| [episode_state_work.rs](/Users/jianxcao/work/fun/crawler-media/crates/store/tests/episode_state_work.rs) | 更新签名并验证其他季的数据量边界 |
| [episode-navigation.test.mjs](/Users/jianxcao/work/fun/crawler-media/web/test/episode-navigation.test.mjs) | 验证下一集选择的可见行为 |

新增测试文件 `/Users/jianxcao/work/fun/crawler-media/crates/store/tests/playable_episode_coverage.rs`。HTTP 用例放在新的 `/Users/jianxcao/work/fun/crawler-media/crates/api/tests/playback_read_regression/episode_publication.rs`，由现有 `main.rs` 注册模块。

统一 Store 和 borrowed connection 的接口。

```rust
pub fn file_coverage_in_library_season(
    &self,
    library: domain::LibraryId,
    media: domain::MediaId,
    season: u32,
    scope: FileReadScope,
) -> Result<Vec<domain::FileCoverage>, StoreError>;
```

### 测试与实施步骤

- [ ] 经 `reserve_transfer` 构造 S01E02 的 pending file，S01E01 使用正常 published file。`WriteScope` 从实际 Library revision 构造。source、destination 和实际复制均在 `/tmp` 内。

  查询 `GET /api/v1/playback/items/{media_id}/episodes?season_number=1`，当前代码会把 pending 的 E02 列为 owned。分别测试带 season 和省略 season 的请求，确认回归测试在正常接口边界失败。

- [ ] Store 测试以同一 reservation 的发布前后为断言对象，使用以下完整用例。

```rust
#[test]
fn reserved_episode_becomes_playable_only_after_publication() {
    use domain::{Media, MediaId, MediaKind, UnitKey};
    use store::library_write::WriteScope;
    use store::test_support::{FixtureFile, publish_fixture_file};

    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let root = temp.path().join("tv");
    std::fs::create_dir_all(&root).unwrap();
    let store = store::Store::open(temp.path().join("data")).unwrap();
    store.create_library(
        MediaKind::Tv, "TV", &[root.to_str().unwrap()], "everyone", true, &[],
    ).unwrap();
    let media_id = MediaId::new();
    store.insert_media(&Media {
        id: media_id,
        kind: MediaKind::Tv,
        title: "Episodes".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }).unwrap();
    let source = root.join("S01E01.mkv");
    std::fs::write(&source, b"media").unwrap();
    let first = publish_fixture_file(&store, &FixtureFile {
        media_id,
        path: source.display().to_string(),
        season: Some(1),
        episode: Some(1),
        ..Default::default()
    }).unwrap();
    let scope = WriteScope {
        library_id: first.library_id,
        config_revision: store.library_config_revision(
            &first.library_id.to_string(),
        ).unwrap(),
    };
    let mut candidate = store.file_coverage_by_id(first.file_id).unwrap().unwrap();
    candidate.file.file_id = domain::LedgerId::new();
    candidate.file.path = root.join("S01E02.mkv");
    candidate.path_key = candidate.file.path.display().to_string();
    candidate.units = vec![UnitKey::new(1, 2)];
    let reserved = store.reserve_transfer(
        &scope, &candidate, &source, library::TransferMode::Copy, 1_000,
    ).unwrap();
    let before = store.file_coverage_in_library_season(
        scope.library_id, media_id, 1, store::FileReadScope::Playable,
    ).unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].file.file_id, first.file_id);

    std::fs::copy(&source, &reserved.file.path).unwrap();
    store.finish_transfer(&scope, &reserved, &reserved.file.version, 2_000).unwrap();
    let after = store.file_coverage_in_library_season(
        scope.library_id, media_id, 1, store::FileReadScope::Playable,
    ).unwrap();
    assert_eq!(after.len(), 2);
    assert!(after.iter().any(|row| row.file.file_id == reserved.file.file_id));
}
```

候选构造沿用 [file_demotion.rs](/Users/jianxcao/work/fun/crawler-media/crates/store/tests/file_demotion.rs) 中现有 Transfer reservation 的做法，全部断言通过正常发布及读取入口完成。

- [ ] 将现有 `file_coverage_in_library` 的三种范围谓词提取为同一模块内的私有函数，两条查询共用。

```rust
fn file_scope_predicate(scope: FileReadScope) -> &'static str {
    match scope {
        FileReadScope::Playable =>
            " AND f.publication_state = 'published' AND f.presence_state = 'present'",
        FileReadScope::Published =>
            " AND f.publication_state = 'published'",
        FileReadScope::AllRecorded => "",
    }
}
```

season SQL 保留 `u.library_id`、`u.media_id`、`u.season` 过滤及 `library_file_units` join，追加上述谓词。

- [ ] 修改 playback 的两个分支。

```rust
let coverage = match season {
    Some(season) => store.file_coverage_in_library_season(
        library, media.id, season, store::FileReadScope::Playable,
    ),
    None => store.file_coverage_in_library(
        library, Some(media.id), store::FileReadScope::Playable,
    ),
};
```

- [ ] 更新全部 season 查询调用者，并补齐下列矩阵。

| 数据状态 | 播放分集清单 |
| --- | --- |
| published + present | 返回对应集及可播放 file ID |
| pending + present | 排除该 file 的覆盖 |
| published + missing 或 unknown | 排除该 file 的覆盖 |
| 一集有两个版本，仅一个 Playable | 仅返回可播放来源 |
| 一个文件覆盖多集 | 每集准确出现一次 |
| S00E00 | 合法返回 |
| Media 可见但可播放集合为空 | 返回 200 和空集合 |
| Media 对当前 User 不可见 | 返回 404 |
| 其他季从 100 个文件增加到 10,000 个 | 单季查询的结果及既有工作量界限通过 |

- [ ] 用 HTTP 返回集合验证 E01 后的自动下一集跳过待发布 E02，选择可播放 E03。完成 E02 的 `finish_transfer` 后再次获取清单，下一集改为 E02。

```sh
python3 scripts/with-build-lock.py cargo test -j4 -p store \
  --test playable_episode_coverage -- --test-threads=4
python3 scripts/with-build-lock.py cargo test -j4 -p store \
  --test episode_state_work -- --test-threads=4
python3 scripts/with-build-lock.py cargo test -j4 -p api \
  --test playback_read_regression episode_publication -- --test-threads=4
python3 scripts/with-build-lock.py pnpm --dir web exec \
  node --test test/episode-navigation.test.mjs
```

- [ ] 全部通过后更新 Self API 的 `owned` 语义并提交。

## Task 6 · BFCache 恢复时重新签发受校验的播放会话

### 选定行为

| 页面事件 | 行为 |
| --- | --- |
| `pagehide` 且 `persisted=true` | 保存当前播放快照，发送 paused progress，暂停本页媒体和心跳 |
| `pageshow` 且 `persisted=true` | 从快照发起一次会话恢复，使用新 SID 和新 stream URL |
| `pagehide` 且 `persisted=false` | 提交终止上报和本次质量数据 |
| SPA 切集或播放器卸载 | 执行现有终止流程，撤销待恢复快照 |
| 用户原先主动暂停 | 新会话就位后保持暂停，用户恢复播放时发送 start |
| 已知管理员结束或恢复接口确认管理员结束 | 显示已有结束提示，等待用户操作 |
| 普通切后台及持续画中画 | 按现有播放与心跳逻辑执行 |

恢复时重新签发覆盖服务端已按 300 秒空闲窗口回收原会话的情况。保存的快照包含逻辑播放单元、显式 file 选择、文件时间位置、用户播放意图、音轨、字幕、画质及原 SID。源版本变化时沿用现有 `source_changed` 处理，采用新会话返回的实际起点和轨信息。

### 文件与接口

| 文件 | 修改职责 |
| --- | --- |
| [video-player.tsx](/Users/jianxcao/work/fun/crawler-media/web/components/player/video-player.tsx) | 绑定页面事件、报告身份切换和恢复参数 |
| [machine.ts](/Users/jianxcao/work/fun/crawler-media/web/lib/player/machine.ts) | 复用 `restart` 的 attempt 和清理语义 |
| [heartbeat.ts](/Users/jianxcao/work/fun/crawler-media/web/lib/player/heartbeat.ts) | 页面挂起期间停发心跳 |
| [api/playback.ts](/Users/jianxcao/work/fun/crawler-media/web/lib/api/playback.ts) | `DecideBody` 增加受校验的恢复参数 |
| [playback/session.rs](/Users/jianxcao/work/fun/crawler-media/crates/api/src/http/playback/session.rs) | 解析恢复参数并映射明确错误 |
| [playback/session_ids.rs](/Users/jianxcao/work/fun/crawler-media/crates/store/src/playback/session_ids.rs) | 暴露受校验的 renewal 接口和错误类型 |
| [session_ids/connection.rs](/Users/jianxcao/work/fun/crawler-media/crates/store/src/playback/session_ids/connection.rs) | 在同一事务校验前驱 SID 并签发下一代 |
| [playback_read_regression/main.rs](/Users/jianxcao/work/fun/crawler-media/crates/api/tests/playback_read_regression/main.rs) | 注册 HTTP 恢复回归模块 |

新增文件。

- `/Users/jianxcao/work/fun/crawler-media/web/lib/player/page-lifecycle.ts`，负责事件订阅和一次挂起/恢复配对。
- `/Users/jianxcao/work/fun/crawler-media/web/test/player-page-lifecycle.test.mjs`，对实际生产事件绑定进行测试。
- `/Users/jianxcao/work/fun/crawler-media/crates/store/src/playback/session_ids/renewal.rs`，负责前驱身份、管理员结束和代次校验。
- `/Users/jianxcao/work/fun/crawler-media/crates/api/tests/playback_read_regression/page_restore.rs`，验证签发、进度与实际 Range 读取。

前端内部接口使用生产事件边界。

```ts
import type { PlayerPhase } from "./machine";

export interface PlaybackPageSnapshot {
  unitKey: string;
  playSessionId: string | null;
  phase: PlayerPhase;
  positionMs: number;
  wantsPlay: boolean;
  announced: boolean;
  endedByAdmin: boolean;
}

export interface PlaybackPageHandlers {
  snapshot(): PlaybackPageSnapshot;
  suspend(snapshot: PlaybackPageSnapshot): void;
  restore(snapshot: PlaybackPageSnapshot): void;
  stop(snapshot: PlaybackPageSnapshot): void;
}

export function bindPlaybackPageLifecycle(
  target: EventTarget,
  handlers: PlaybackPageHandlers,
): () => void;
```

仅将页面事件和快照配对抽到该模块。音轨、字幕、画质及显式 source 继续由组件现有状态和 refs 持有，恢复入口在当前 `unitKey` 相同时读取。

会话接口增加 `resume_from_play_session_id?: string`。登录态 BFCache 自动恢复携带原 SID，手动开始通过原来的正常签发流程。Store 增加以下语义独立的操作，共用当前签发 SQL。

```rust
#[derive(Debug, thiserror::Error)]
pub enum PlaybackRenewalError {
    #[error("previous playback session unavailable")]
    PreviousUnavailable,
    #[error("previous playback target changed")]
    TargetChanged,
    #[error("playback superseded by a newer session")]
    Superseded,
    #[error("playback ended by administrator")]
    EndedByAdmin,
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl Store {
    pub fn renew_play_session(
        &self,
        user_id: domain::UserId,
        device_id: &str,
        previous_session_id: &str,
        resolved: &crate::ResolvedResume,
        now: i64,
    ) -> Result<String, PlaybackRenewalError>;
}
```

该错误类型位于 `store::playback::session_ids::PlaybackRenewalError`。`session_ids.rs` 声明内部 `renewal` 模块，API 使用此完整类型路径。

这是自动恢复需要的授权及竞态约束。当前 `ended_by_admin` 进度响应有设备级时间窗口，自动恢复需要按原 SID 查询持久事实，覆盖页面缓存期间结束的会话。

### 测试与实施步骤

- [ ] 为实际 `page-lifecycle` 生产入口写事件测试。以下用例验证发送的协议事件和状态机的结果，fake snapshot 使用固定逻辑目标。

```js
test("BFCache preserves progress and restarts at the saved position", () => {
  const target = new EventTarget();
  const reports = [];
  let state = {
    ...initialPlayerState,
    phase: "playing",
    startMs: 42_000,
    attempt: 2,
  };
  const snapshot = {
    unitKey: "media:1:2",
    playSessionId: "old-session",
    phase: "playing",
    positionMs: 42_000,
    wantsPlay: true,
    announced: true,
    endedByAdmin: false,
  };
  const dispose = bindPlaybackPageLifecycle(target, {
    snapshot: () => snapshot,
    suspend: (saved) => reports.push({
      event: "progress", paused: true,
      play_session_id: saved.playSessionId, position_ms: saved.positionMs,
    }),
    restore: (saved) => {
      state = playerReducer(state, { type: "restart", startMs: saved.positionMs });
    },
    stop: (saved) => reports.push({
      event: "stop", play_session_id: saved.playSessionId,
    }),
  });
  const hide = new Event("pagehide");
  Object.defineProperty(hide, "persisted", { value: true });
  target.dispatchEvent(hide);
  assert.deepEqual(reports, [{
    event: "progress", paused: true,
    play_session_id: "old-session", position_ms: 42_000,
  }]);
  const restore = new Event("pageshow");
  Object.defineProperty(restore, "persisted", { value: true });
  target.dispatchEvent(restore);
  assert.equal(state.phase, "session-starting");
  assert.equal(state.startMs, 42_000);
  assert.equal(state.attempt, 3);
  assert.equal(state.session, null);
  dispose();
});
```

测试文件导入 Node 的 `assert`、`test`、实际 `bindPlaybackPageLifecycle` 和 `playerReducer`。补充真实组件接入测试，使页面事件确实触发会话请求和卸载清理。

- [ ] 实现事件配对。只在 persisted pagehide 保存快照，pageshow 消费一次。消费前核对当前 `unitKey`、原 SID、phase 和管理员终止状态。playing、buffering、seeking 可恢复，error、consent、ended、idle 保留当前界面。首次签发仍在途且 SID 为空时，恢复该次已由用户发起的请求。事件绑定的 dispose 撤销快照和监听器。重复 pageshow、普通首次 pageshow、已换集后旧 pageshow 均保持当前会话。

- [ ] 挂起时先保存 `wantsPlay` 和当前位置，再暂停 video。使用 `reportPlaybackProgressOnUnload` 发送带递增 sequence 的 `event='progress', paused=true`。暂停产生的媒体事件通过挂起状态过滤。停止 heartbeat timer。

- [ ] 在同一 Library writer 事务中实现受校验的 renewal。

  1. 按 `(user_id, device_id, previous_session_id)` 查原 binding，校验 Media 与 UnitKey 一致。
  2. 按该 SID 查询 live session 与 `playback_logs.admin_ended`，匹配完整 User、device 和 SID。命中后返回 `EndedByAdmin`。
  3. 若该设备已有更高 generation 的会话，返回 `Superseded`，避免缓存页面覆盖更新的播放选择。
  4. 当前原 SID 为 pending、active 或因空闲关闭时均可继续。绑定已被保留期清理时返回 `PreviousUnavailable`。
  5. 复用现有签发过程对实际 file、coverage、source version、Library revision、可见性进行校验，并分配新 generation。
  6. 原 SID 的校验与新 SID 的登记共用该事务。抽出所需的事务内签发实现，由原正常签发和 renewal 两个入口调用。
  7. 新 SID 激活时继续使用现有“关闭更低 generation”逻辑。管理员结束与激活竞争时，正常 heartbeat 的事务内管理员检查继续生效。

- [ ] 在 HTTP 层严格解析新增字段。空字符串、错误类型和非法 UUID 返回 `400 playback.invalid`。错误映射采用下表，前端按结构化 code 处理。

| renewal 结果 | HTTP |
| --- | --- |
| PreviousUnavailable | `404 playback.session_missing` |
| TargetChanged | `409 playback.resume_target_changed` |
| Superseded | `409 playback.resume_superseded` |
| EndedByAdmin | `409 playback.ended_by_admin` |
| 新来源当前不可读 | 沿用播放选择的 404 契约 |
| 存储内部错误 | 500 并记录完整服务端上下文 |

- [ ] 在组件恢复入口使用已有 `restart`。

  - 捕获原 SID 后清空旧 reporting identity、`reportedStartRef` 和旧 sequence，并使恢复请求拥有当前 attempt。
  - 请求带保存的 `start_ms` 和 `resume_from_play_session_id`，复用 capability、显式 file、音轨、字幕和画质。
  - 新响应到达后设置新 SID，sequence 从新会话的初值递增，使用新 stream URL。
  - 保留 `wantsPlay=false` 的用户选择。暂停恢复的 pending 会话在真正 playing 后上报 start。
  - start、progress 的异步响应携带发出时 SID。只有仍匹配当前报告身份时才影响组件状态。
  - pagehide 时同步推进页面恢复代次，签发请求捕获发出时的代次。错误或会话回包在 effect 被取消、unitKey 已改变、attempt 或页面代次已落后时丢弃。
  - `ended_by_admin` 走现有 `onEndedByAdmin`，其他恢复错误进入可手动重试的错误状态。
  - 成功绑定新 SID、换集、卸载或进入终态时消费并清理本次恢复参数。用户在错误页主动重试时使用普通签发流程，重新读取当前目标与权限。
  - `progress='local'` 的分享场景使用该分享原有会话入口及本地进度，同样按页面事件保存位置。renewal 字段只发送到提供成员 SID 的登录态入口。
  - 普通终止上报消费当前 reporting identity。pagehide 与紧随其后的卸载共享该终止标记，使 stop 和质量数据按同一会话归档。

- [ ] 添加端到端 HTTP 行为测试，使用已有 `make_state`、`seed_member`、`seed_library`、`add_ledger` 和离线 fixture。

| 场景 | 断言 |
| --- | --- |
| 已 start 的 SID 收到 paused progress | SID 保持可读取，观看时长按暂停规则计算 |
| 正常恢复 | SID 改变，generation 增大，返回起点正确，新 URL 的 `Range: bytes=0-0` 返回 206 及预期字节 |
| fake clock 推进 301 秒并执行 session sweep | 原 SID 关闭，renewal 成功，新 URL 可读取 |
| 管理员结束后立即恢复 | 409 `playback.ended_by_admin` |
| 管理员结束、归档、超过 10 秒后恢复 | 同样返回管理员结束，覆盖设备时间窗口之外 |
| 原 SID 属于其他 User/device | 404 |
| 更高 generation 已签发 | 409 `playback.resume_superseded` |
| 页面原本暂停 | 新会话保留暂停意图，手动播放后 start 使用新 SID |
| 连续两次 BFCache 往返 | 每次读取新 SID，sequence 不混用 |
| 普通 pagehide 和 SPA unmount | 正常 stop，服务端归档原会话 |
| 源文件被替换或权限被收回 | 采用当前来源校验结果，错误时可明确退出恢复流程 |

- [ ] 定向验证后，在真实浏览器测试导航离开和后退，记录 `pageshow.persisted`。分别覆盖播放中、主动暂停、静置超过 300 秒和管理员结束。确认 Network 中的新 SID、Range 响应、进度与轨选择。

```sh
python3 scripts/with-build-lock.py pnpm --dir web exec node --test \
  test/player-page-lifecycle.test.mjs test/player-machine.test.mjs \
  test/player-heartbeat.test.mjs test/player-source-transitions.test.mjs
python3 scripts/with-build-lock.py cargo test -j4 -p api \
  --test playback_read_regression page_restore -- --test-threads=4
python3 scripts/with-build-lock.py cargo test -j4 -p api \
  --test playback_event_identity -- --test-threads=4
python3 scripts/with-build-lock.py cargo test -j4 -p store \
  --test playback_session_ids -- --test-threads=4
```

- [ ] 在 Self API 中记录恢复字段、错误码与自动恢复语义，完成本任务提交。

## 集成验收与合并条件

### 处理审查中出现的间歇失败

审查阶段曾完成一次 workspace 全量执行，645 个 target，4,492 个测试通过、1 个失败、28 个忽略。失败为 `marker_boundary_publication` 中的 `job_scope::ordinary_adaptive_does_not_publish_to_outside_job_references`，超时发生在 `pair_cache.rs` 的完成等待。单独重跑在 0.22 秒内通过。此项仍需定位。

相关文件为 [marker_boundary_publication.rs](/Users/jianxcao/work/fun/crawler-media/crates/api/tests/marker_boundary_publication.rs)、[pair_cache.rs](/Users/jianxcao/work/fun/crawler-media/crates/api/tests/marker_boundary_publication/pair_cache.rs) 和 [job_scope.rs](/Users/jianxcao/work/fun/crawler-media/crates/api/tests/marker_boundary_publication/job_scope.rs)。

- [ ] 先在原来的 5 秒边界复跑完整 target，连续执行 10 次，任一次失败即保留该轮证据并进入定位。

```sh
python3 scripts/with-build-lock.py python3 - <<'PY'
import subprocess
for attempt in range(1, 11):
    print(f"marker_boundary_publication attempt {attempt}", flush=True)
    result = subprocess.run([
        "cargo", "test", "-j4", "-p", "api",
        "--test", "marker_boundary_publication",
        "--", "--test-threads=4",
    ])
    if result.returncode:
        raise SystemExit(result.returncode)
PY
```

- [ ] 超时时收集当前 ProbeJob 状态、analysis 队列、仍占有的 dispatch ID、相关 Library 生命周期、fake clock 值和 worker shutdown 结果。输出仅包含本地 fixture 的标识。
- [ ] 区分两类等待。job 未到终态时检查 analysis 准入与 fake clock 唤醒，job 已终态但 dispatch ID 未释放时检查 worker/lease 生命周期。
- [ ] 若发现测试全局状态或 fixture 清理竞态，先加入可重复触发的生命周期回归，再修复对应所有权或清理逻辑。若发现生产调度缺陷，将修复及回归作为独立提交纳入本轮。
- [ ] 保留原始业务断言和超时边界，用可重复证据说明根因及修复。若仍无法解释此次失败，将它明确列为合并前未完成项。

### 定向与最终命令

完成六项定向验证及间歇失败定位后执行 crate 级测试和静态检查。

```sh
python3 scripts/with-build-lock.py cargo test -j4 -p store -- --test-threads=4
python3 scripts/with-build-lock.py cargo test -j4 -p api -- --test-threads=4
python3 scripts/with-build-lock.py cargo fmt --all -- --check
python3 scripts/with-build-lock.py cargo clippy -j4 --all-targets -- -D warnings
python3 scripts/with-build-lock.py pnpm --dir web lint
python3 scripts/with-build-lock.py pnpm --dir web typecheck
python3 scripts/with-build-lock.py pnpm --dir web test
python3 scripts/with-build-lock.py pnpm --dir web build
```

最后执行一次 workspace 全量测试。

```sh
python3 scripts/with-build-lock.py --exclusive cargo test -j8 \
  --workspace --no-fail-fast -- --test-threads=8
git diff --check
```

浏览器验收按 [E2E runbook](/Users/jianxcao/work/fun/crawler-media/docs/testing/e2e-runbook.md) 配置临时实例，刷新版本戳并核对 health commit，使用本地媒体 fixture。验证结果记录到工作记录或临时报告，提交包含实现、测试及对应契约文档。

### 最终验收清单

- [ ] 父 Library 删除经过迁移准入，250 个嵌套文件、对应 Media 和用户事实完整。
- [ ] 16 个重配置操作在容量 2 下全部完成，故障操作可通过现有 retry 恢复。
- [ ] 所有 root 写入口识别既有与待执行 roots 计划的冲突，拒绝请求具有事务原子性。
- [ ] 相同语言保存返回当前状态，已受理工作继续。
- [ ] 单季、全部季和下一集使用一致的 Playable 集合。
- [ ] BFCache 短时、超过 300 秒、暂停和管理员结束四条路径均通过行为验证。
- [ ] 间歇测试失败已有根因结论和对应验证。
- [ ] fmt、clippy、Web 检查、变更 crate 测试及最终 workspace 套件通过。
- [ ] 最终相对 `origin/main` 复审覆盖新增的数据库准入、失败收尾和自动恢复权限校验。

满足以上条件后，可将本轮审查结论更新为可合并。当前记录的三个 P1 以及尚未解释的全量测试失败仍阻止给出该结论。


## 2026-11-19 实施记录

六项修复已按 public seam 回归实施。删除准入、16 库重配置等待、根目录持久化占用及 failed 计划修正、同语言保存幂等、Playable 分集和 BFCache SID renewal 均已有通过的定向验证。最终复审发现二次 BFCache 发生在续签在途时会丢失前驱，已补两项组件 red→green 回归并通过 scoped 复审。

关键取舍：沿用当前 `feat/storage-redesign` 工作区；任务 2 的 driver claim 先于任务 3 的计划修正；分集 scope 复用私有查询谓词；页面生命周期仅负责缓存事件配对。二次恢复若先前续签已成功而回包过期，保留前驱校验并展示 Superseded 错误，用户手动重试。此边界可能要求额外点击，但不会自动覆盖新设备播放或绕过管理员终止。

已完成验证：Web 全套 1,028 测试、typecheck、build、Rust fmt 和严格 clippy；root typed Conflict 的两个既有断言已更新并定向通过。全 crate 重复执行在定位旧断言并确认规模夹具持续推进后终止，统一由最后一次独占 workspace 执行覆盖。规模夹具通过约 228,000 个持久化步骤构造 9,500 条目，较长等待来自 SQLite fsync，未调整超时或持久化配置。

真实浏览器已验证 paused、playing、342 秒缓存及管理员终止四种 `persisted=true` 导航；前三种携带旧 SID 续签并使用新 SID，管理员终止后 54 秒返回得到 409 `playback.ended_by_admin`、页面暂停并显示管理员提示。关闭旧 SID 后恢复另有 HTTP fake-clock + sweep 回归。完整浏览器证据保存在本轮临时报告，测试凭据不进入仓库。

历史 marker 间歇失败以原 5 秒边界连续 10 轮（400 测试）未复现，另一次加诊断后 40 测试通过；已核查调度去重、fixture 隔离和 fake clock 唤醒，仍未建立根因。保留原边界并仅在失败时输出 job/analysis/dispatch/lifecycle/clock/shutdown 诊断。最终 workspace 结果另行补充，不能将未复现描述为已定位。

最终 workspace 当前已发现 `fingerprint_benchmark::lifecycle::completion::media_waits_for_its_successor_analysis_while_other_media_completes` 的 frame entry 2 秒等待超时；diagnostics 已保留，正在按原边界核查 fixture gate 与 analysis 生命周期。此项尚未确认通过，不能宣称完整套件全绿。


### 最终验证补充

独占 workspace 执行完成：646 个 target、4,525 通过、8 失败、28 忽略，退出码 101。失败集中在四个 target；保留原始结果，未将该次执行记为全绿，也未重复整套 workspace。

- `library_presence`：根冲突由 SQL UNIQUE 转为 typed `Conflict`，更新既有断言；完整 target 7/7 通过。
- `library_removal_subscribes`：夹具在开始删除前显式完成根目录迁移，完整 target 5/5 通过。
- `file_rename_durable`：多 root 夹具排空建库迁移，保留发布期间 `WriteBusy` 断言；完整 target 16/16 通过。
- `fingerprint_benchmark`：发现迟到 capture 工作可以把后继 Analysis 的 queued 状态改成没有 claim token 的 running。`start_probe_unit` 改为明确阶段的 CAS，Analysis 仅通过自身 claim API 启动；真实交接回归先失败后通过。完整 target 13/13 通过，并以原 2 秒等待边界连续十轮 130/130 通过。`marker_boundary_publication` 完整 target 40/40 通过。

阶段校验仅改变启动准入，Metadata/Fingerprint 的生产入口各自传固定阶段；调用夹具显式声明阶段。未增加兼容分支，未修改探测默认并发、远程间隔或超时。追加只读复审确认阶段盗用已封住，未发现新的 P1/P2。历史 marker 间歇失败无法从旧日志证明同一根因，保留这一证据边界。

Web 最终 1,028/1,028、typecheck、lint、build 通过；lint 有仓库既有 warnings。Rust fmt、严格 clippy `--all-targets -- -D warnings`、diff check 和修改的 Rust 文件/函数硬尺寸检查通过。其余阶段调用相关定向结果记录在执行 ledger；提交前再次核对。
