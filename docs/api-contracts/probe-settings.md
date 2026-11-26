# 媒体信息探测设置

管理员在「设置 → 刮削与整理 → 媒体信息探测」调整实例的探测速度。设置持久化在 app.db 的 `settings` 表中，键为 `probe.settings`，一次保存完整 JSON；其他刮削设置不受影响。

## HTTP

`GET /api/v1/settings/probe` 返回当前保存值；尚未保存时返回默认值。`PUT /api/v1/settings/probe` 完整替换设置，三个字段均为必填整数，不接受未知字段。

```json
{
  "metadata_workers": 1,
  "remote_concurrency": 1,
  "remote_cooldown_ms": 500
}
```

成功返回标准 `{ "ok": true, "data": { ... } }`，`data` 与上面形状一致。两种方法都要求管理员权限，普通成员返回 403。

| 字段 | 默认 | 范围 | 行为 |
| --- | --- | --- | --- |
| `metadata_workers` | 1 | 1–8 | 同时处理媒体流信息的文件数，包含本地和远程文件 |
| `remote_concurrency` | 1 | 1–8 | 媒体信息、声纹、图片与章节后台读取共享的远程活动读取上限 |
| `remote_cooldown_ms` | 500 | 0–60000ms | 远程读取结束后到下一次准入的间隔；0 表示不等待 |

并发上限 8 限制读取与子进程资源开销；间隔最大一分钟覆盖保守限速需求，避免误填极长等待。越界、小数、负数、缺少字段和未知字段返回 `400 probe.invalid`，原有配置不变。存储失败返回 `500 store.error`，写入准入繁忙遵循全局 503 约定。

## 生效时机

启动时加载持久化配置。保存成功后无需重启：后续媒体信息任务和远程读取按新设置准入；更改读取间隔也会让正在排队的读取重新计算等待时间。

活动读取不会被中断。降低并发时，必须等现有活动数降到新上限以下才进入后续任务；不会在已有两个任务运行时因其中一个结束就违反新上限 1。同优先级媒体信息文件按派发顺序等待准入，后加入的文件不能越过队首等待文件。声纹仍使用独立的单 worker 队列，`metadata_workers` 不改变它。

## STRM 后台读取暂停

管理员可在同一页面立即暂停或恢复 STRM 后台读取。`GET /api/v1/settings/probe/remote-reads` 返回 `{ "paused": false }` 形状的 `data`，默认启用读取。`PUT` 接受同样形状的请求并立即应用，`paused` 必须为布尔值。设置以 `probe.remote_reads` 持久化。

暂停覆盖媒体信息、声纹、图片和章节的后续远程读取。已获准的读取可完成，等待读取在恢复后继续，并沿用当前并发与间隔。重启仍应用已保存的状态。这个开关只控制后台读取。

## 运行期探测队列控制

管理员另可通过 `/api/v1/probe/*`（非管理员返回 403；`resume` 与 `cancel` 同时受实例只读/维护 `write_gate` 约束）查看与控制后台探测队列：

- `GET /api/v1/probe/status`：返回 `{paused, remote_reads_paused, queued_metadata, running_metadata, queued_fingerprint, running_fingerprint, pending_obligations, total_pending, libraries: [{library_id, library_name, queued_count, running_count, obligation_count, total_pending}]}`（`libraries` 仅列出 `total_pending > 0` 的库）。
- `POST /api/v1/probe/pause` / `POST /api/v1/probe/resume`：切换进程内后台探测派发开关并返回与 `/probe/status` 相同形状的状态。该开关不写入 `app.db`；生产 `serve` 每次启动/重启默认初始化为 `paused: true` 以避免积压队列自动冲击远程挂载，需经 `/probe/resume` 或库级 `POST /api/v1/libraries/{id}/media-info/refresh` 恢复（单条目 `POST /api/v1/libraries/{id}/items/{item_id}/probe` 不受该暂停限制），`/probe/resume` 还会立即补派一批待处理 `file_probe_obligations`。
- `POST /api/v1/probe/cancel`：接受空请求体（全局取消）或严格 JSON `{"library_id"?: string}`（未知字段或非法 JSON 返回 `400 probe.invalid`），仅取消 `status = 'queued'` 的待处理探测单元并清理对应 `file_probe_obligations`，**不中断**已在运行（`running`）的单元；返回 `{cancelled_units, cleared_obligations}`。

## 验证

- `crates/api/tests/probe_settings_runtime.rs`：默认读取行为、HTTP 保存/读回/参数错误/权限、重建实例后持久化、媒体信息并发热升降、后加入文件不越过旧等待文件、共享远程并发热修改及启动读取间隔。
- `crates/api/tests/probe_queue_control_and_restart_pause.rs`、`crates/api/tests/probe_paused_manual_and_cancel.rs`：生产启动默认暂停、`/probe/{status,pause,resume,cancel}` 权限与队列统计、单条目探测豁免及仅取消排队项语义。
- `crates/marker/tests/media_read.rs`：共享准入、读取间隔、取消、本地读取不被远程阻塞、运行时缩短间隔。
- `web/test/probe-settings.test.mjs`：真实组件加载、范围校验、保存和失败后重试。
