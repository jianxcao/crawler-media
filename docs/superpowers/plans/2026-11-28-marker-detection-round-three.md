# 标记检测、云同步与插件投递修复方案

> 实施时可使用 `superpowers:executing-plans`，按下列复选框逐项执行。审查基线为 `92eab65f`，功能分支的受审提交为 `6b2af806`。

**目标**　管理员发起的检测、同步和查询应返回与实际执行一致的结果。插件事件在短暂读取故障后应能继续投递，插件构建应选中本项目生成的组件。

**实现边界**　SQLite 保存任务意图、投递回执和标记。Rust API 负责把这些事实转换为 HTTP 回执。React 负责观察当前 Library、Media、季的状态。CLI 按 Cargo 构建所选包识别产物。

**依据**　`docs/api-contracts/self.md` 第 47、53 行，`docs/adr/0013-wasm-plugin-runtime.md` 第 21 行，仓库根 `AGENTS.md`。后文所有路径均相对仓库根目录。

## 实施顺序与验证约束

先做任务 1 和 2，两者共用单集入队逻辑。任务 3 和 4 可分别验收，修改云端发布代码时顺序合入。其余任务可独立实施。每项先从实际调用入口得到预期失败，再修复并重跑同一用例，测试与实现组成一次提交。

使用本地 HTTP 桩、可控的探测器和时钟。探测 worker 默认 1，远程并发默认 1，间隔默认 500ms。每条验证命令从仓库根执行。下列命令组供每项红、绿阶段调用，Rust 用例添加在已经登记的套件中。`review_round2.rs` 和 `review_fixes.rs` 均已超过 740 行。在各文件内分别用 `#[path = "review_round2/episode_admission.rs"] mod episode_admission;` 和 `#[path = "review_fixes/cloud_sync.rs"] mod cloud_sync;` 登记新子模块，子模块通过 `super` 复用现有夹具。

```sh
marker_tests() {
  python3 scripts/with-build-lock.py cargo test -j4 -p api --test api_suite_4 "marker_standalone_control::$1" -- --test-threads=4
}
event_tests() {
  python3 scripts/with-build-lock.py cargo test -j4 -p api --test api_suite_3 'plugin_event_dispatcher::delivery_retry::' -- --test-threads=4
}
web_tests() {
  python3 scripts/with-build-lock.py node --test web/test/marker-standalone-control.test.mjs
}
cli_tests() {
  python3 scripts/with-build-lock.py cargo test -j4 -p plugin-cli --test build_cmd_regressions -- --test-threads=4
}
```

## 1. 让显式单集检测在关闭自动检测的 Library 中执行

**定位**　`crates/api/src/probe_manager/fingerprint_control.rs` 按是否命中媒体信息缓存选择 `fingerprint_probe` 或 `media_probe`。前者在 `crates/store/src/probe_tasks/connection.rs` 的可选工作准入处被自动开关拒绝。后者可以入队，但 `crates/api/src/probe_manager/probe.rs` 只通过 `marker_refresh_id` 识别显式刷新，于是读取完媒体信息便可成功结束。单集调用没有这个整季标识。

- [ ] 在新子模块 `crates/api/tests/marker_standalone_control/review_round2/episode_admission.rs` 增加 `explicit_episode_runs_with_automatic_fingerprint_disabled`。关闭 `enable_fingerprint`，保留两个在位 TV 文件和空队列。分别对有、无当前版本媒体信息缓存的文件发起 `POST .../markers/detect`，指定单个 `file_id`、`episode` 和 `mode=recapture`。用现有可阻塞探测器等到声纹阶段，释放后从诊断接口读取该文件的终态。
- [ ] 执行 `marker_tests 'review_round2::episode_admission::explicit_episode_runs_with_automatic_fingerprint_disabled'`。现状下热缓存分支返回 `409`，冷缓存分支在媒体信息阶段后跳过声纹。两个分支的验收断言都是 `queued == true`，且目标文件实际经过声纹采集。以同 Library 的普通媒体信息请求作为对照。
- [ ] 在 `ProbeJobUnitSpec` 和持久化单元已有的 `force_fingerprint`、`overwrite_markers` 等字段上明确表示单集显式请求，先核对所有写入点，确保普通强制探测可以区分。将同一判断用于创建、`complete_media_probe_and_enqueue_fingerprint`、worker 准入和 `dispatch.rs` 的恢复路径。单集只发布自身目标。整季的 `marker_refresh_id` 继续表示批次结果的原子发布。
- [ ] 重跑上述用例及 `python3 scripts/with-build-lock.py cargo nextest run --build-jobs 4 --test-threads 4 -p api -E 'test(library_optional_probe_switches)'`。覆盖重启派发、Library 删除或停用时的拒绝，以及普通探测仍受自动开关控制。提交 `fix(api): preserve explicit episode detection through capture`。

## 2. 在返回“已在运行”前核实已有任务的用途

**定位**　`crates/api/src/probe_manager/media_info.rs` 和 `fingerprint_control.rs` 都使用 `ledger:{file_id}`。`persist_single_episode_marker_job` 在占用冲突时只核对键，普通 `media_probe` 也会成为标记检测的成功回执。它没有承诺重新采集或替换标记。

- [ ] 在新子模块 `crates/api/tests/marker_standalone_control/review_round2/episode_admission.rs` 增加 `plain_media_probe_conflicts_with_episode_recapture`。复用 `GatedTracksProbe`，以 `force=false` 发起普通媒体信息刷新，阻塞在实际探测阶段，再对同文件发起 `recapture`。另设相同显式请求重复提交的对照组。
- [ ] 执行 `marker_tests 'review_round2::episode_admission::plain_media_probe_conflicts_with_episode_recapture'`。占用组期望 `409` 和 `body["error"]["code"] == "markers.conflict"`，当前得到 `200` 和普通任务 ID。重复提交显式检测应得到 `already_running == true`，且 `job.id` 等于第一次提交的任务 ID。
- [ ] 在现有 `library_write_with_error` / `with_write_tx` 内校验活动任务及其目标单元，使用任务 1 明确的标记检测意图。普通媒体信息占用返回 `Conflict`。符合当前接口同作用域去重约定的显式任务返回 `AlreadyRunning(existing.id)`。沿用当前暂停状态下替换休眠任务的事务边界。
- [ ] 重跑该用例和 `marker_tests 'followup_round3::'`，同时核实跨 Library 场景与首次新入队的回执。提交 `fix(api): reject unrelated ledger work in marker receipts`。

## 3. 对显式云同步区分上游故障、冷却和无匹配

**定位**　`crates/api/src/theintrodb_lookup.rs` 通过 `Option<StoredFileMarker>` 把 TheIntroDB 故障和无匹配合并为 `None`。`crates/api/src/http/marker_control/cloud.rs` 遇到 `None` 跳过目标，最终面板会把无效密钥、配额耗尽或服务故障显示为“未发现可更新标记”。

- [ ] 在 `crates/api/tests/marker_standalone_control/review_fixes.rs` 为本地 `StubIntroDbServer` 加入可编排的响应，在新子模块 `crates/api/tests/marker_standalone_control/review_fixes/cloud_sync.rs` 增加 `explicit_cloud_sync_reports_upstream_failure`。分别返回上游 401、429、503 和有效的 `no_submissions` 响应。首次错误后在冷却期再次发起显式同步，再用测试时钟越过冷却期并提供一个可用标记。
- [ ] 执行 `marker_tests 'review_fixes::cloud_sync::explicit_cloud_sync_reports_upstream_failure'`。三个上游错误现均可得到 `200`、`updated_episodes=0`。验收时前三种均返回可识别的错误，确实无匹配才返回 `200`、计数零。恢复后的请求应发布标记并返回计数一。
- [ ] 为底层查找结果区分 `Ok(Some(marker))`、`Ok(None)` 和带原因的 `Err`。在 `record_lookup_error` 继续按现有规则更新冷却与负缓存。显式接口将认证故障映射为可读的上游配置错误，将限流与服务暂不可用映射为可重试错误，并将冷却情况传给调用方。使用现有错误信封，在 `docs/api-contracts/self.md` 补充所选状态码。播放查询仍可将可恢复错误降级为当前章节结果。
- [ ] 添加批次断言，第一项写入后第二项遇到 503 时，第一项可读且章节截图仍能获知有待处理的工作，请求会报告失败。重跑 `marker_tests 'review_fixes::'`。提交 `fix(api): report upstream failures during explicit cloud sync`。

## 4. 按云端发布事务实际写入的行数计数

**定位**　`crates/api/src/http/marker_control/cloud.rs` 先在读连接检查 `locked`，然后进入 Library writer。若期间管理员锁定同一版本的标记，`crates/store/src/library/markers.rs` 的 UPSERT 影响零行，但 `put_file_marker_checked` 返回 `Ok(())`，HTTP 仍增加 `updated_episodes`。用仓库原始 UPSERT 已复现手工行保留、云端更新零行。

- [ ] 在新子模块 `crates/api/tests/marker_standalone_control/review_fixes/cloud_sync.rs` 增加 `cloud_sync_skips_marker_locked_before_commit`。控制同一 Library 的写入顺序，让云同步预检读到未锁，再让管理员的锁先提交，最后放行云端写入。用写入准入的同步点协调，无需按毫秒猜测时序。断言 `updated_episodes == 0`，并通过 GET 检查 `source == "manual"`、`locked == true` 及原时间范围。
- [ ] 执行 `marker_tests 'review_fixes::cloud_sync::cloud_sync_skips_marker_locked_before_commit'`。当前实现能保留手工标记，但计数为一。
- [ ] 让 `upsert_marker_row` 返回 `tx.execute(...)` 的影响行数或其布尔结果。沿 `crates/store/src/library/markers/connection.rs` 的受校验发布路径传至 API，零行返回 `CloudPublishOutcome::Skipped`，仅成功写入才调度对应截图更新。保持文件版本及覆盖检查与 UPSERT 在同一事务；人工 PUT 保留现有 HTTP 响应。
- [ ] 运行同一用例，另断言未锁标记的首次同步计数一，源版本变化仍返回 `409`。提交 `fix(store): count only applied cloud marker updates`。

## 5. 让章节刷新进度跟随当前 Library

**定位**　`crates/api/src/http/library_chapters/refresh.rs` 已按 Library 创建整季任务，但其 `probe_status` 仍按 `(media_id, season)` 读取最近一次刷新。两个 Library 收录同一集时，B 刚完成的任务可以让仍在运行的 A 提前结束轮询。实际消费者是 `web/components/library-item/use-item-chapters-and-probe.ts`。

- [ ] 在 `crates/api/tests/marker_standalone_control/followup_round3.rs` 增加 `chapter_probe_status_follows_refresh_library`。复用双 Library 共享 TV 夹具，为各库创建独立整季刷新，让较早的 A 保持运行、较晚的 B 完成。分别以管理员身份 GET 两个库的 `.../probe-status?season=1`。
- [ ] 执行 `marker_tests 'followup_round3::chapter_probe_status_follows_refresh_library'`。期望 A 返回 A 的 ID 及 `active=true`，B 返回 B 的 ID 及 `active=false`。当前 A 的响应指向 B。
- [ ] 在 `crates/store/src/probe_tasks/connection.rs` 的整季刷新查询中接收 `library_id`，用任务单元对应的 `library_files` 验证目标归属。保留 `kind='marker_refresh'`，对 `season IS NULL` 的多季任务按该库文件实际覆盖的季匹配。在 HTTP 处理函数中传递已经解析的 `library_id`。增加一个只有 B 有任务时 A 返回 `job=null` 的对照。
- [ ] 运行同一用例和 `marker_tests 'followup_round3::'`，核实多季刷新、单集普通探测和跨库读取。提交 `fix(api): scope chapter refresh progress to its library`。

## 6. 等待当前诊断请求完成后再轮询

**定位**　`web/components/library-item/marker-control-panel.tsx` 每两秒调用 `load()`，而 `load()` 会中止正在进行的读取。当同一季的诊断持续超过两秒，终态响应会反复被丢弃，面板仍显示进行中。

- [ ] 在 `web/test/marker-standalone-control.test.mjs` 增加同季慢请求用例。首次 GET 返回 `active=true`，下一次返回 `deferred()`。推进两个以上轮询周期，再让已发出的请求返回 `active=false`。断言页面离开进行中状态，`onChanged` 收到一次通知。另在慢请求期间切季，旧季结果应被丢弃。
- [ ] 执行 `web_tests`，确认新增用例能在原实现上重现终态丢失。
- [ ] 复用 `web/lib/single-flight-poll.ts` 的 `startSingleFlightPoll(round, 2000)`。让 `round` 等待本次 `load` 完成，成功时用响应的 `active` 决定是否继续。失败可在下一轮重试。切季和卸载时停止计时器并取消当前请求，保留 `scopeGenRef` 与显式刷新所需的响应顺序检查。
- [ ] 重跑 `web_tests` 和 `python3 scripts/with-build-lock.py pnpm --dir web typecheck`。提交 `fix(web): wait for marker diagnostics before polling again`。

## 7. 冷缓存加载组件时保留插件事件的重试机会

**定位**　`crates/api/src/plugins/events_delivery.rs` 已读取并验证 `PluginVersionMetadata`。组件缓存未命中时，`crates/api/src/plugins/component_cache.rs` 会再次读同一版本，把 `ReadBusy` 等临时错误变成 `Failure::InvalidComponent`。经 `hooks_runtime/invoker.rs` 返回 `None` 后，dispatcher 将已领取的事件直接标为 `dead_letter`。

- [ ] 在 `crates/api/tests/plugin_event_dispatcher/delivery_retry.rs` 增加首轮冷缓存用例。安装有效插件并创建 `download.after` 事件，确保首次加载 policy 组件。用可控的读准入在取得版本、准备加载组件时制造一次短暂不可读，随后恢复。观察 `get_delivery_receipt` 及本地 webhook。任何一次瞬时故障都不能令首次领取进入 `DeadLetter`，解除故障后应到达 `Delivered`。
- [ ] 执行 `event_tests`。确认旧代码在第二次版本读取失败时直接转为死信。已有 grants 或首次版本读取故障用例覆盖的是更早的阶段。
- [ ] 将本次解析已验证的 `PluginVersionMetadata` 传给 policy 缓存加载路径。缓存键仍使用插件 ID、包哈希和入口，缓存未命中时用 `read_component_entry_bytes` 读取对应产物。若有必需的 Store 读取则保留 `StoreError` 类型，由 dispatcher 按现有退避与尝试上限处理。保留撤权和确定无效组件的终态处理。
- [ ] 重跑 `event_tests` 并覆盖缓存命中、撤权、真实组件无效、持续暂态错误达到尝试上限。提交 `fix(api): preserve transient errors during plugin policy resolution`。

## 8. 插件构建只匹配 Cargo 选中包的 WASM

**定位**　`tools/plugin/src/build_cmd.rs` 把 Cargo 输出中所有以 `.wasm` 结尾的文件交给组件匹配逻辑。真实的离线 Cargo 构建中，单组件 `policy` 引用同时输出 `rlib` 和 `cdylib` 的路径依赖 `helper`，编译成功后 CLI 却因 `helper.wasm` 没有对应 manifest 入口而退出。

- [ ] 在 `tools/plugin/tests/build_cmd_regressions.rs` 增加 `build_ignores_dependency_cdylib`。用本地路径依赖生成上述两个包，执行 `plugin build`，断言命令成功且声明入口的内容为 `policy` 的构建产物。再用依赖 target 名碰巧匹配预构建组件入口的夹具，断言该入口内容保留。
- [ ] 执行 `cli_tests`。原实现会产生 `did not match any component id or entry stem`，或错误匹配预构建入口。
- [ ] 用相同 `--manifest-path` 的 `cargo metadata --format-version 1 --no-deps` 确定本次构建选择的包。普通 manifest 选择其对应的根包，虚拟 workspace 使用与 `cargo build` 相同的默认成员。读取 `compiler-artifact.package_id`，先过滤所选包，再执行既有的组件 ID、入口文件名与歧义校验。同步更新模拟 Cargo 的测试夹具，使其提供真实格式的包标识。
- [ ] 重跑 `cli_tests`，核实已声明的多个产物仍逐一映射，真实歧义在写入前报错，路径依赖支持离线构建。提交 `fix(plugin-cli): filter component artifacts by Cargo package`。

## 集成验收

以下断言分别放在对应测试中。错误响应使用 `common::json_body` 读取完整信封，成功响应用 `common::json_data` 读取 `data`。

```rust
// 任务 1 与 2，分别在声纹探测器进入采集后、普通探测占用时断言。
assert_eq!(admitted["queued"], true);
assert_eq!(conflict_status, StatusCode::CONFLICT);
assert_eq!(conflict_body["error"]["code"], "markers.conflict");

// 任务 3 与 4，分别对确定无匹配和并发人工锁检查回执。
assert_eq!(no_match_status, StatusCode::OK);
assert_eq!(no_match["updated_episodes"], 0);
assert!(upstream_failure_status.is_server_error() || upstream_failure_status.as_u16() == 429);
assert_eq!(after_lock["updated_episodes"], 0);
assert_eq!(persisted_marker["source"], "manual");
assert_eq!(persisted_marker["locked"], true);

// 任务 5，同一 Media 同一季的两个 Library 在同一时刻有不同进度。
assert_eq!(status_a["job"]["id"], submitted_a["job"]["id"]);
assert_eq!(status_a["active"], true);
assert_eq!(status_b["job"]["id"], submitted_b["job"]["id"]);
assert_eq!(status_b["active"], false);

// 任务 7 与 8，分别在恢复投递和 CLI 成功退出后检查结果。
assert_eq!(recovered.status, OutboxDeliveryStatus::Delivered);
assert_eq!(std::fs::read(&policy_entry).unwrap(), policy_bytes);
assert_eq!(std::fs::read(&prebuilt_entry).unwrap(), prebuilt_bytes);
```

```js
// 任务 6，推进两个轮询周期后再完成同季的慢响应。
await act(async () => slowResponse.resolve({ ...activeDiagnostics, active: false }));
assert.equal(changedCount, 1);
assert.doesNotMatch(collectText(panel.toJSON()), /进行中/);
```

- [ ] 执行 `marker_tests ''`、`event_tests`、`web_tests`、`cli_tests`，查看运行数以确认新增用例均已登记。每个 Rust 变更 crate 再按 `AGENTS.md` 运行其定向套件。
- [ ] 执行下列命令。`nextest` 全量在上述任务合入后运行一次，若有失败，记录失败用例、断言、受审基线对照及本机工具版本。

```sh
python3 scripts/with-build-lock.py cargo fmt --all -- --check
python3 scripts/with-build-lock.py pnpm --dir web test
python3 scripts/with-build-lock.py --exclusive cargo nextest run --build-jobs 8 --test-threads 8 --workspace --no-fail-fast --show-progress=none --status-level=slow --final-status-level=fail
```
