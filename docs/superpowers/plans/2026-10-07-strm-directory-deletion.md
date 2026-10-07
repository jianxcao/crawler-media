# STRM 目录删除与变动防抖实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 完善文件监控（FsWatcher）对媒体目录及文件删除的处理：支持递归目录删除并级联清理台账及关联媒体，同时明确 45 秒宽限期（Grace Period）的作用范围（仅在变动场景下保护，真正删除/整目录删除时直接移除台账）。

**Architecture:**
1. 在 `crates/api/src/fs_watcher.rs` 中，当监听到路径不存在（删除事件）时：
   - 检查台账 `ledger` 中是否存在以该路径为前缀的记录（针对整目录删除场景）或完全匹配该路径的记录（单个文件删除场景）。
   - 如果找到匹配的 ledger 条目：
     - 若为确定的目录删除或用户直接删除，直接从台账删除记录（调用 `store.delete_ledger_path`）；
     - 检查被删除记录所属的 `media_id`，如果该 Media 对应的 ledger 已被全部删除（remaining == 0），则清理其播放记录、标记、收藏项以及该 `Media`（若为非订阅保留的空条目）。
2. 在 `StrmGraceTracker` 中明确变动与删除的边界：
   - 变动防抖：当文件发生内容/URL变更时，防止短时间重复刮削和探测；
   - 删除处理：当检测到路径真实已不存在且用户删除时，直接进入清理流程，不再因为 45 秒延迟阻碍条目删除。

**Tech Stack:** Rust, notify, tokio, SQLite (rusqlite/store)

---

### Task 1: 提取并实现台账级联删除逻辑（支持单个文件或整个目录）

**Files:**
- Modify: `crates/api/src/fs_watcher.rs`
- Test: `crates/api/tests/management/strm_directory_deletion.rs`

- [ ] **Step 1: 编写失败测试**
  测试模拟：在 media 库中存在一个目录，内含 1 个或多个 `.strm` 文件并已入账。直接删除该剧/电影的根目录，触发 `handle_fs_events` 或对应的删除处理函数后，验证对应的 `ledger` 记录被清除，且若该媒体无其他文件，媒体关联信息也一并清理。

- [ ] **Step 2: 运行测试验证失败**
  `cargo test -p api --test strm_directory_deletion`

- [ ] **Step 3: 实现递归目录/文件删除检测与清理**
  在 `crates/api/src/fs_watcher.rs` 中：
  - 当 `!path.exists()` 时，查询所有 `ledger` 记录中以 `path` 作为完全匹配或路径前缀的记录。
  - 删除所有匹配的 `ledger` 记录。
  - 检查受影响的 `media_id` 集合，对于剩余 ledger 为 0 的条目，级联清理标记、播放状态和 media。

- [ ] **Step 4: 运行测试验证通过**
  `cargo test -p api --test strm_directory_deletion`

- [ ] **Step 5: 运行全项目测试**
  `cargo test -p api`

- [ ] **Step 6: Git commit**
