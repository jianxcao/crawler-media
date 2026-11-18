# 0011: 探测阶段状态与退避持久化 (Probe Stage Retry & State)

## 上下文

媒体探测与声纹分析分别使用高优先级 `metadata` 和低优先级 `fingerprint` 工作队列。片头、片尾和媒体信息可以独立成功或失败；持久化子阶段状态与退避时间，避免页面或播放器请求在失败后重复拉流。

## 决策

1. **按阶段跟踪状态**：`library.db.probe_stage_state` 以 `(file_id, context_key, stage)` 为联合主键，记录 `metadata`、`intro`、`outro` 阶段状态，文件事实来自 `library_files`（`crates/store/src/schema/v1/library.sql`）。
2. **持久化退避策略**：失败时记录 `failure_count` 与 `next_retry_at_ms`（重试间隔：60s, 300s, 900s, 3600s，最多 4 次自动重试，之后需手动触发）。
3. **支持 Partial 终态**：在 `probe_jobs` 表支持 `partial` 终态（片头成功但片尾未就绪），避免当作无退避的失败或整体成功。
4. **生命周期与文件绑定**：删除文件事实时在同事务显式清除 `probe_stage_state`；该表本身没有外键，不能称为 DDL 级联保证（`crates/store/src/ledger_cleanup.rs`）。
5. **轻量对比摘要**：`probe_season_comparison_history` 记录整季声纹对比摘要，供后续跳过未变化的声纹比对。
## 扫描入库探测义务与重启恢复

`file_probe_obligations.file_id` 外键引用 `library_files` 并 `ON DELETE CASCADE`；扫描经 `insert_scanned_files` 在同一事务发布文件/覆盖/摘要并登记义务（`crates/store/src/scan_probe_obligations.rs`、`library_write`）。

- 即使扫描在元数据反查或部分阶段中途退出，已登记的 ledger 仍具备未完成的 probe 义务。
- 调度器每 5 秒一轮、每轮最多取 100 条待处理义务（`crates/api/src/scan_probe_dispatch.rs`）。
- ACK 规则：探测请求返回 `Queued{job_id}` 时只对该 job 确认（`ack_scan_probe_obligation_for_work`）；`AlreadyRunning` 按当前服务它的工作确认；`Complete` 仅在义务仍为当前版本时确认。派发失败或返回失败退避（`Waiting`，不是 `AlreadyRunning`）时**保留义务**等待下一轮重试。
- 文件事实已不存在或已被标记 missing 时直接 ACK，避免永远占用 100 条窗口；源恢复时重新登记义务（`crates/store/src/ledger_presence/lifecycle.rs`）。
- 删除文件事实时级联清理对应 probe obligation；防止已派发 worker 迟到发布仍需生命周期租约与提交检查，不能只由外键删除推断 worker 已停止。

## Watcher 源变化与派生结果恢复

Watcher 用 `library_files.source_version` 与锁外读取的 STRM 源观察比较内容身份，源摘要由统一的 `library::observe_source` 生成。首次收到路径事件、进程重启或内存宽限期状态丢失都不构成源变化证据；持久源版本相同就保留流信息、章节和标记，不重复扫描。内存宽限期只负责删除/重建防抖，不能覆盖 SQLite 的文件事实。

确认源已变化后，Watcher 通过 `Store::replace_file_source` 在同一 Library 事务中校验预期文件版本与配置 revision、推进文件 revision/source_version、失效旧版本派生结果、取消该文件旧源的排队/执行单元并登记 `file_probe_obligations`，保留既有终态历史。新源义务携带新文件 revision 的确认标识；即使两次变化发生在同一毫秒，旧源派发后迟到的 ACK 也不能消费新源义务，旧工作也不能以 `AlreadyRunning` 代替新源的探测。人工锁定标记也不能跨源版本继续生效。事务失败时保留原结果并交给 ScanCoordinator 重试，不能先分散删除缓存再依赖详情页访问触发探测。

已缺失流信息的文件通过 `POST /api/v1/libraries/{id}/media-info/refresh` 的 `mode: "missing"` 恢复。请求必须携带 UUID `request_id`，每页最多处理 50 个文件；响应 `next_cursor` 非空时，以同一 `request_id` 和 `after: next_cursor` 继续。该入口持久化补探测工作，保留仍有效的结果；历史 Job 的成功终态不能代替当前文件版本的有效结果。

探测、取消、重启及真实拉流需要分别验证；阶段成功不等于全部阶段完成。分析调度与片头片尾的精确来源约束见[标记优化设计](../design/marker-optimization.md)，验证方法见[标记验收方法](../testing/marker-accuracy-runbook.md)。
