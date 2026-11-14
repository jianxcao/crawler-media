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

探测、取消、重启及真实拉流需要分别验证；阶段成功不等于全部阶段完成。分析调度与片头片尾的精确来源约束见[标记优化设计](../design/marker-optimization.md)，验证方法见[标记验收方法](../testing/marker-accuracy-runbook.md)。
