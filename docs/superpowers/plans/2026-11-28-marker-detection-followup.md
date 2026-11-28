# feat/marker-detection 第二轮修复方案

> 实施时可用 `superpowers:executing-plans` 逐项执行。复选框用于跟踪实际修复进度。

**目标**　修复 `92eab65f..b96367a1` 审查中仍可触发的 7 个问题。首先防止取消或编辑作用到错误的文件，再修正检测进度、云端同步回执和插件行为。

**设计**　取消范围在 HTTP 入口验证。面板中的异步操作绑定到发起时的 Library、Media 和季。提交回执由本次 SQLite 写入决定，季诊断展示同范围内仍活动的任务。插件投递沿用现有带领取凭证的有限重试。

**技术栈与依据**　Rust、axum、SQLite、React、Node 测试运行器、WASM Component。行为以[标记 API](../../api-contracts/self.md)、[队列控制契约](../../api-contracts/probe-settings.md)、[插件运行时 ADR](../../adr/0013-wasm-plugin-runtime.md)和[插件作者指南](../../plugins/authoring.md)为准。

**执行约束**　每项先从 HTTP、组件或 CLI 入口跑出失败，再改实现，重跑同一用例。普通测试经 `scripts/with-build-lock.py`，最多两组并行。保留默认远程读取并发 1 和间隔 500 ms。API 测试沿用已登记的 `api_suite_3`、`api_suite_4` 子模块。先合入任务 3 再做任务 4，避免把季诊断查询误用于提交回执。[仓库规则](../../../AGENTS.md)

## 1 · P1　明确拒绝空的取消范围字段

**位置**　`crates/api/src/http/fingerprint_settings.rs` 的 `CancelFingerprintRequest` 和 `cancel`。HTTP 回归放入 `crates/api/tests/marker_standalone_control/review_round2.rs`。

- [ ] **复现。** 复用 `seed_two_libraries_shared_tv` 与 `ControllableFingerprint::blocking()`，准备一项正在采集和一项排队的工作。发出 `{"file_id":" "}`。现有入口会将其化为无范围的取消，且默认中止活动采集。对 `{"library_id":"实际库 ID","file_id":""}` 重复测试，确保有效字段不能掩盖另一个无效字段。错误响应使用 `common::json_body` 读取。
- [ ] **实现。** 对允许省略的选择字段使用 Serde 的字段缺省值，字段出现时直接反序列化其具体类型。三个 ID 执行 `trim` 并拒绝空串，显式 `null` 返回 `400 probe.invalid`。`season` 出现时读取 `u32`，仍接受 0。待所有字段验证完毕才构建 `FingerprintCancelScope`、进入写事务和查找活动令牌。空请求体、`{}` 及设置页发出的 `{"include_running":true}` 继续表示全局取消。
- [ ] **验证。** 对三个 ID 分别覆盖空串、纯空白与 `null`，另测非法季号。收到 400 后，在释放采集屏障前核对两项工作的状态，释放后核对两者均可完成。再分别发送有效文件 ID 和显式全局请求，确认两种作用范围。

```rust
assert_eq!(status, StatusCode::BAD_REQUEST);
assert_eq!(body["error"]["code"], "probe.invalid");
assert!(store.get_probe_job(&running_id).unwrap().unwrap().is_active());
assert!(store.get_probe_job(&queued_id).unwrap().unwrap().is_active());
```

运行 `python3 scripts/with-build-lock.py cargo test -j4 -p api --test api_suite_4 marker_standalone_control:: -- --test-threads=4`。

## 2 · P2　阻止上一季的异步操作修改当前面板

**位置**　`web/components/library-item/marker-control-panel.tsx` 负责选季、诊断、操作反馈和编辑表单。组件回归放入 `web/test/marker-standalone-control.test.mjs`。

- [ ] **复现。** 用现有 `deferred()` 挂起 S1 的检测 POST，切到 S2 并返回仅含 `file2` 的诊断，再释放 S1 POST。已复现的请求顺序为 S1 → S2 → S1，选择器停在 S2。此时点击「清除」，请求携带 `file1` 和 `season=1`。
- [ ] **实现。** 让外层持有选季状态，按下列 React `key` 挂载持有诊断、编辑草稿、`busy`、反馈和异步回调的子组件。切换后子组件从空诊断开始，旧组件的清理函数中止其 GET 并置失效标志。检测、取消、保存、清除在每次 `await` 后、触发 `load` 或 `onChanged` 前检查本实例仍有效。这样切回同一季也会创建新实例。

```tsx
<MarkerControlPanelBody
  key={JSON.stringify([libraryId, mediaItemId, isTv, isTv ? season : null])}
  {...bodyProps}
/>
```

- [ ] **验证。** S2 的 GET 尚未返回时，上一季的行已经无法编辑或清除。S1 的成功或错误到达后，S2 仍显示 `file2`，点击清除只发送 `file2, season=2`。补测 S1 → S2 → S1 时第一次 S1 操作迟到、关闭面板时保存迟到，以及同一季内乱序 GET。保留已选章节与手选季的现有行为。

运行 `python3 scripts/with-build-lock.py node --test web/test/marker-standalone-control.test.mjs` 和 `python3 scripts/with-build-lock.py pnpm --dir web typecheck`。

## 3 · P2　按 Library 接收整季检测并返回本次任务

**位置**　`crates/api/src/http/marker_control/detect.rs` 接收入队结果，`crates/api/src/probe_manager/refresh.rs` 持久化整季任务，`crates/api/src/probe_manager/fingerprint_control.rs` 持久化单集任务。去重键在 `crates/api/src/probe_manager.rs`，唯一约束在 `crates/store/src/schema/v1/library.sql`。回归放入 `crates/api/tests/marker_standalone_control/review_round2.rs` 和前述面板测试。

- [ ] **复现。** 在 A、B 两个 Library 中为同一 Media 的 S1 登记不同文件。保持 A 的任务活动，分别对两库 POST `{"season":1,"mode":"recapture"}`，省略 `file_id` 和 `episode`。当前两个请求生成同一个 `scope_key`，已在分支的 SQLite 唯一索引复现第二次插入失败。B 没有其他任务时，接口会返回 `queued=false, already_running=false`，页面却显示「已提交」。
- [ ] **实现。** Library 条目入口将已校验的 `lib_id` 传入 `ProbeManager`，并核对所有单元属于该库。给这种整季请求生成含 Library、Media、季的 `scope_key`。保留 `probe_unit_active` 对每个文件的占用保护。在相同作用域被重复提交时从持久记录取回实际任务 ID。遇到某文件被不同范围占用时返回明确冲突，持久化故障则沿现有错误通路返回。检查章节刷新等共用调用方各自的范围，再决定其键，避免同一批文件被重复执行。
- [ ] **修正回执。** 内部区分新建、复用活动任务、冲突和写失败。将创建或复用的 ID 传回 handler，按 ID 取摘要并核对归属。一次单集请求命中多个版本文件时，在文档中约定单一 `job` 字段的选择顺序，例如按文件 ID 稳定排序后优先取本次新建的任务。前端仅在 `queued` 为真时显示「已提交」，复用任务则显示其运行状态。
- [ ] **验证。** 两库各收到本库任务 ID，任务单元仅含本库文件。紧接着重复提交 A，应返回 A 原 ID。取消 B 后 A 可继续完成。另设同库中较新的无关单集任务，验证单集提交和重复提交的摘要始终属于本次请求。

```rust
assert_eq!(receipt_a["queued"], true);
assert_eq!(receipt_b["queued"], true);
assert_ne!(receipt_a["job"]["id"], receipt_b["job"]["id"]);
assert_eq!(repeat_a["already_running"], true);
assert_eq!(repeat_a["job"]["id"], receipt_a["job"]["id"]);
```

运行 `python3 scripts/with-build-lock.py cargo test -j4 -p api --test api_suite_4 marker_standalone_control:: -- --test-threads=4`，再运行任务 2 的组件测试。

## 4 · P2　季诊断优先展示仍在运行的任务

**位置**　`crates/store/src/probe_tasks/fingerprint_control.rs` 的 `latest_marker_job_for_season` 和 `crates/api/src/http/marker_control.rs` 的 `active`。HTTP 回归放入 `crates/api/tests/marker_standalone_control/review_round2.rs`。

- [ ] **复现。** 同一 Library 同一季使用两个有媒体信息缓存的文件，先让 E1 单集检测保持 `running`，再提交并按文件取消 E2。请求该季诊断。现有 SQL 已在内存 SQLite 上复现返回较新的 `cancelled` 行，此时接口会报告 `active=false`，页面停止轮询。
- [ ] **实现。** 将季诊断方法的名称和注释改为表达「活动任务优先」。继续应用 Library 归属、任务种类和季的原有过滤，同一状态组内沿用原排序。GET 用选中行生成 `job` 和 `active`，POST 使用任务 3 中本次提交的 ID。

```sql
ORDER BY (j.status IN ('queued', 'running')) DESC,
         j.created_at_ms DESC, j.rowid DESC
LIMIT 1
```

- [ ] **验证。** E1 活动时 GET 返回 `active=true` 和 E1 的 ID。E1 也结束后返回 `active=false` 以及最近创建的合格任务。另一 Library 即使存在更晚的活动任务，也不能改变本库结果。组件接到上述状态序列时继续轮询，直到最后一项工作结束。

运行 `python3 scripts/with-build-lock.py cargo test -j4 -p api --test api_suite_4 marker_standalone_control:: -- --test-threads=4`。

## 5 · P2　云端标记写入失败时返回错误

**位置**　`crates/api/src/http/marker_control/cloud.rs` 的 `publish_direct_cloud_marker` 和 `run_theintrodb_only`。写入准入映射在 `crates/api/src/http/store_wait.rs`。HTTP 回归放入 `crates/api/tests/marker_standalone_control/review_fixes.rs`。

- [ ] **复现。** 使用已有 `StubIntroDbServer` 返回一个有效标记，在返回云端数据前用屏障占住 Library writer，让发布超过现有 1 秒准入预算。选取单个在位、未锁定的目标文件。当前发布将错误归为 `false`，接口返回 200、`updated_episodes=0`，面板提示未发现可更新标记。
- [ ] **实现。** 发布函数改为返回能携带 HTTP 错误的 `Result`。保留「已更新」与「跳过」两种正常结果，将 `with_foreground_store` 的拒绝原样返回。读取覆盖范围、当前 Media、锁定状态时传播 Store 错误，保留已锁定、云端确无记录等预期跳过情形。对于一季多文件，按文件提交；若后续文件失败，先通知此前已成功写入的章节截图任务，再返回失败并记录对应文件。
- [ ] **验证。** 同一 HTTP 测试检查 `503 runtime.busy`、`Retry-After`、库中标记为空。释放 writer 后重复请求得到 `updated_episodes=1`，读回来源为 `theintrodb`。保留「全季已锁定」和「云端确无匹配」返回 200、更新 0 项的对照。

```rust
assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
assert_eq!(response.headers()["retry-after"], "1");
assert_eq!(common::json_body(response).await["error"]["code"], "runtime.busy");
```

运行 `python3 scripts/with-build-lock.py cargo test -j4 -p api --test api_suite_4 theintrodb_only -- --test-threads=4`。

## 6 · P2　领取插件事件后保留可恢复的投递

**位置**　`crates/api/src/plugins/events_delivery.rs` 的 `resolve_delivery_targets`、`deliver_claimed_item` 和 `settle_notify_failure`。`crates/api/src/plugins/events.rs` 负责读取授权。Store 的 `schedule_retry` 和 `mark_dead_letter` 已校验 `claim_nonce`。回归放入 `crates/api/tests/plugin_event_dispatcher.rs`。

- [ ] **复现。** 创建一个订阅 `download.after` 且具备双重授权的插件，在 `subscribe.db` 写入事件，并用 `seed_event_fanout` 建立 delivery。仅对领取后读取插件或 grants 的步骤注入一次 `StoreError::ReadBusy`。第一次 drain 后通过 `get_delivery_receipt` 观察，当前实现会在 guest 尚未执行时将 delivery 标记为 `dead_letter`。
- [ ] **实现。** `plugin_holds_dual_event_grants_for_event` 保留 `Result<bool, StoreError>`。让目标解析区分暂时读取故障、已证实无权限和永久无效的输入。读池繁忙或读超时时使用当前凭证、`retry_delay_ms` 和 `schedule_retry` 写回同一 delivery。继续按 `MAX_EVENT_ATTEMPTS`（当前 5 次）封顶。记录数据库、事件、插件、尝试次数和错误类型。实际撤权、停用和无效 manifest 保留现有终态。
- [ ] **验证。** 第一次 drain 后状态为 pending、尝试数为 1、本地 webhook 未收到该事件。读故障恢复并到达 `next_attempt_at` 后，原 `(event_id, plugin_id)` 为 delivered、尝试数为 2。本例只收到一次回调。另测真实撤权时零回调、持续故障按上限进入死信，以及旧领取凭证无法更新已经被重新领取的记录。

```rust
assert_eq!(first.status, OutboxDeliveryStatus::Pending);
assert_eq!(first.attempt_count, 1);
assert_eq!(recovered.status, OutboxDeliveryStatus::Delivered);
assert_eq!(recovered.attempt_count, 2);
```

运行 `python3 scripts/with-build-lock.py cargo test -j4 -p api --test api_suite_3 plugin_event_dispatcher:: -- --test-threads=4`。

## 7 · P2　多组件插件构建时保护每个入口的产物

**位置**　`tools/plugin/src/build_cmd.rs` 的 `build_wasm_components`，CLI 回归放入 `tools/plugin/tests/build_cmd_regressions.rs`。双组件清单示例见 `docs/plugins/authoring.md`。仓库已有 `crates/plugin-host/tests/fixtures/policy.wasm` 和 `crates/plugin-host/tests/fixtures/task_fixture.wasm` 可作不同 world 的输入。

- [ ] **复现。** 按作者指南创建带 `Cargo.toml`、hook 和 task 两个入口的临时项目。预先放置可识别的 task 产物，让 Cargo 只产出 hook。执行 `plugin build` 后同时核对退出状态和 task 字节。审查中已复现 CLI 报告成功，task 入口内容变为 hook。
- [ ] **实现。** 从 Cargo 的 `compiler-artifact` 消息取得实际 `.wasm` 路径。对于单组件、单产物可直接匹配。多个组件时只接受能唯一对应入口的产物，例如不同 Cargo target 和对应入口文件同名；保留项目单独提供并通过校验的其他组件。无法确定映射时，在写入任一入口前报错，指出组件 ID 和候选产物。先收集、验证整组映射，再通过现有安全路径检查写入。把匹配条件与独立预构建的用法记入作者指南。
- [ ] **验证。** 成功时解开 `.cmplugin`，分别比对 hook 和 task 入口与预期 fixture 的字节。在没有足够信息的用例中，命令失败且两个入口均保持原内容。再跑单组件脚手架、纯预构建项目、相对目录以及符号链接拒绝的已有回归。

```rust
assert_eq!(std::fs::read(&hook_entry).unwrap(), expected_hook);
assert_eq!(std::fs::read(&task_entry).unwrap(), expected_task);
```

运行 `python3 scripts/with-build-lock.py cargo test -j4 -p plugin-cli --test build_cmd_regressions -- --test-threads=4`。再运行 `python3 scripts/with-build-lock.py cargo test -j4 -p plugin-cli -- --test-threads=4`。

## 实施收尾

- [ ] 每项修复将对应的 red/green 用例与代码放在同一提交，对照相应复现步骤核实外部可见结果。文中命令从仓库根目录执行。
- [ ] 运行 `python3 scripts/with-build-lock.py cargo fmt --all -- --check`、`python3 scripts/with-build-lock.py pnpm --dir web test` 和 `python3 scripts/with-build-lock.py pnpm --dir web typecheck`。
- [ ] 各项定向用例通过后，运行一次 `python3 scripts/with-build-lock.py --exclusive cargo nextest run --build-jobs 8 --test-threads 8 --workspace --no-fail-fast`。记录实际结果和仍需人工确认的情况。
