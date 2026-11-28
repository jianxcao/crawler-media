# feat/marker-detection 审查整改实施方案

> 实施者可用 `superpowers:executing-plans` 逐项执行。复选框表示实施进度。

**目标**　修复 `92eab65f..fa176e02` 审查中的 2 项 P1 和 8 项 P2，使镜像可构建、成员只能看到所属 Library 的任务、取消和暂停作用于用户选定的工作，并让可恢复的故障得到重试。

**架构**　任务归属沿 `probe_job_units.file_id → library_files.library_id` 解析。设置以 `app.db` 为准，声纹准入按同一提交顺序发布。插件事件沿用所属数据库的 outbox 和已有投递去重机制。

**技术栈**　Rust、axum、Tokio、SQLite、React、pnpm、Docker 多阶段构建、插件开发 CLI。

**依据**　[仓库规则](../../../AGENTS.md)、[标记 API](../../api-contracts/self.md)、[探测控制](../../api-contracts/probe-settings.md)、[SQLite ADR](../../adr/0008-sqlite-system-of-record.md)、[插件运行时 ADR](../../adr/0013-wasm-plugin-runtime.md)。

## 顺序与边界

| 顺序 | 对应审查项 | 可独立验收的结果 |
|---|---|---|
| 第一批 | 1、2 | 干净镜像能打包 Web；成员诊断隔离通过。这两项可并行。 |
| 第二批 | 3、4、5、6、8 | 从条目面板发起、取消和查看检测时，Library、季、文件及最新设置一致。3、5 共用一个组件，宜依次修改。 |
| 独立修复 | 7、9、10 | 插件事件读取故障可以恢复；插件在新环境和相对目录中都能构建。9、10 共用构建入口，宜依次修改。 |

每项先用公开入口得到预期失败，再做最小实现，最后重跑同一用例及相邻行为。普通 Rust 构建经 `scripts/with-build-lock.py`，每组 `-j4`，全局最多两组。保留媒体信息 worker 默认 1、远程读取并发 1、间隔 500 ms。代码片段给出关键条件或测试断言，测试数据由所列套件的现有夹具提供。

## 1 · P1　恢复 Docker 的 Web 构建

**位置**　`Dockerfile` 的 `web` 阶段。依赖关系见 `web/package.json`、`web/pnpm-lock.yaml`、`sdk/plugin-ui` 和 `sdk/plugin-ui-kit`。

- [ ] **先复现。** 从仓库根运行 `python3 scripts/with-build-lock.py docker build --target web --progress=plain .`。若基础镜像和 registry 可达，旧版本会在安装本地依赖时失败。上次审查已在隔离目录复现同一依赖布局的冻结安装失败。
- [ ] **恢复目录布局。** 保留 `WORKDIR /web`，在冻结安装前复制两个完整包。SDK 从 `src` 导出模块，需要包源码和清单一起进入安装层。仍在依赖安装之后复制整个 `web/`，这样只改页面时可复用安装层。

```dockerfile
COPY web/package.json web/pnpm-lock.yaml ./
COPY sdk/plugin-ui/ /sdk/plugin-ui/
COPY sdk/plugin-ui-kit/ /sdk/plugin-ui-kit/
RUN corepack enable && pnpm install --frozen-lockfile
```

- [ ] **验收。** 重跑同一 Docker Web target，要求冻结安装和 `pnpm build` 都通过，并确认 `/web/dist/index.html` 已生成。最终完整镜像再验证静态首页与 `/api/v1/health`。两者回答不同问题，保留各自结果。

## 2 · P1　按 Library 隔离标记任务诊断

**位置**　`crates/store/src/probe_tasks/fingerprint_control.rs`、`crates/api/src/http/marker_control.rs`、`crates/api/src/http/marker_control/detect.rs`。诊断汇总在 `crates/api/src/http/marker_control/render.rs`。测试放入 `crates/api/tests/marker_standalone_control/review_fixes.rs` 或同套件的新子模块。

- [ ] **先复现。** 在可见 Library A 和私有 Library B 为同一 Media、同一季各登记文件。先给 A 创建任务，再给 B 创建较新的任务。以只有 A 权限的成员请求 A 的 `GET .../markers?season=1`，确认旧代码会选中 B。再测试 A 还没有任务、B 已有任务的情况。

```rust
assert_eq!(diagnostics["job"]["id"], job_a);
// A 有这一季的文件，但只有 B 创建过任务的用例
assert!(diagnostics_without_a_job["job"].is_null());
assert_eq!(diagnostics_without_a_job["active"], false);
```

- [ ] **改查询与两个调用方。** 给 `latest_marker_job_for_season` 的 Store 和连接方法加上 `library_id: domain::LibraryId` 参数。沿任务单元检查文件归属，再执行原有排序和 `LIMIT 1`。GET 使用已校验的 Library；管理员 `POST .../detect` 的任务回执也使用请求中的 Library。保留现有任务种类、季号、状态与排序条件。
- [ ] **保护整条摘要。** `probe_job_json_enriched` 会返回整项任务的计数、错误和耗时。仅有一个单元属于 A 还不足以公开混合任务。最小实现是只返回有成员、且所有成员均可归属于 A 的完整任务。以下谓词附加在既有查询上，`?3` 绑定 Library ID。

```sql
AND EXISTS (SELECT 1 FROM probe_job_units u WHERE u.job_id = j.id)
AND NOT EXISTS (
    SELECT 1 FROM probe_job_units u
    LEFT JOIN library_files f ON f.file_id = u.file_id
    WHERE u.job_id = j.id AND (f.library_id IS NULL OR f.library_id <> ?3)
)
```

- [ ] **检查条目边界。** 对当前 Library 根本没有的 Media，复用条目详情的归属及缺失响应；对已归属当前库、但查询季没有文件的 Media，保留空季结果。用 TV 和 Movie 分别验证，防止用另一个可见库探测私有条目是否存在。
- [ ] **验收。** 分别验证成员和管理员回执、其他库任务较新、A 无任务、混合归属与普通单库。A 的响应不得含 B 的任务 ID、错误、进度或汇总。测试独立任务时可用不同文件的单集任务，避免被整季刷新已有的活动任务去重约束挡住。

## 3 · P2　条目面板取消操作携带 Library ID

**位置**　`web/components/library-item/marker-control-panel.tsx`，测试在 `web/test/marker-standalone-control.test.mjs`。`web/lib/api/probe.ts` 已支持 `library_id`。

- [ ] **先复现。** 从 A 的面板点击「停止本季」和文件行的停止按钮，在 API 客户端边界记录序列化后的请求体。断言本季请求包含 A，文件请求还包含对应的 `file_id`。
- [ ] **修复。** 在 `handleCancel` 共用请求中加入 `library_id: libraryId`，复用已有的 Media、季和文件过滤。设置页的全局取消入口继续使用自身的请求范围。

```ts
await cancelFingerprintQueue({
  library_id: libraryId,
  media_id: mediaItemId,
  ...(isTv ? { season } : {}),
  ...(targetFileId ? { file_id: targetFileId } : {}),
  include_running: true,
});
```

- [ ] **验收。** A、B 有相同 Media/季的独立单集任务时，从 A 停止本季，只终止 A 的匹配任务；B 的任务仍能完成。复用第 2 项的两库数据，在 API 端断言每个任务的最终状态。

## 4 · P2　取消仍在等待媒体信息的显式单集检测

**位置**　`crates/store/src/probe_tasks/fingerprint_control.rs` 的 `select_fingerprint_targets`。任务来源见 `crates/api/src/probe_manager/fingerprint_control.rs` 中的 `enqueue_single_episode_marker_detect`。测试沿用 API 的 `marker_standalone_control` 套件。

- [ ] **先复现。** 用可控 `MediaProbe` 占住唯一的 metadata worker。为没有媒体信息缓存的另一个文件发起单集检测，确认其处于 `media_probe / metadata / queued`，再按 `file_id` 取消。旧行为回执为 0，释放 worker 后仍会采集声纹。
- [ ] **修复筛选条件。** 将下列条件并入现有 fingerprint、analysis 匹配条件，复用事务内现有状态清理、任务计数及内存派发清理。同步检查已投递到内存队列的 ID 在 worker 领取时会复查持久状态。

```sql
OR (u.stage = 'metadata' AND u.status = 'queued'
    AND (j.kind = 'marker_refresh'
         OR (j.kind = 'media_probe' AND u.force_fingerprint <> 0)))
```

- [ ] **验收。** `cancelled_units == 1`，释放 worker 后目标文件不会进入采集，且能立即重新提交。重复取消返回 0。对照普通排队的媒体信息任务、正在运行的 ffprobe，以及 `include_running: false` 的活动采集，分别验证它们可以继续完成。

## 5 · P2　区分章节切换与用户手动选季

**位置**　`web/components/library-item/marker-control-panel.tsx`、`web/components/library-item/item-chapters-section.tsx`，测试在 `web/test/marker-standalone-control.test.mjs`。

- [ ] **先复现。** 用 `seasons=[1,2]`、`selectedSeason=1` 和已选剧集挂载面板，手动选第 2 季，等 React effect 完成。上次审查已复现最终值回到 1。将该用例加入已有组件套件。

```ts
await act(async () => selector().props.onChange({ target: { value: "2" } }));
assert.equal(selector().props.value, 2);
```

- [ ] **修复。** 将「外部条目或章节季变化」与「当前季从可选列表消失」拆开处理。前者只在 `libraryId`、`mediaItemId`、`selectedSeason` 等外部标识变化时同步；后者用函数式 `setSeason`，当前选择仍有效时原样返回。重渲染产生内容相同的新 `seasons` 数组也应保留手选季。
- [ ] **验收。** 切到第 2 季后，诊断 GET 和「检测本季」POST 都发送 `season=2`，属于第 1 季的单集操作隐藏。外部章节真正切换季时面板随之同步。覆盖特别篇 `season=0`、空季列表与乱序 GET，确保旧季回包不会覆盖当前季。

## 6 · P2　使声纹设置的持久化与运行态按同一顺序生效

**位置**　`crates/api/src/http/fingerprint_settings.rs`、`crates/api/src/probe_manager.rs`、`crates/api/src/probe_manager/fingerprint_control.rs`。参考现有 `http/probe_settings.rs` 的发布顺序和 `probe_manager/settings_publish_tests.rs` 的启动竞争测试。

- [ ] **先复现。** 用屏障控制两个管理员请求。A 提交并发 4 后在退出 app writer 的边界延迟返回；B 提交并发 1 和暂停并完成；随后放行 A。通过 `/settings/probe/fingerprint` 与 `/probe/fingerprint/status` 验证持久值和运行值。屏障在测试进程内工作，避免靠重复并发或延长超时撞中时序。
- [ ] **写锁内只发布准入状态。** `PUT`、pause、resume 共用保存路径。`app.put_setting` 成功后，在同一个 `with_app` 闭包内按顺序发布 `paused`、worker 上限及版本。写失败时保持原来的运行值。pause/resume 仍在同一次 writer 获取中读取最新 worker 配置。
- [ ] **锁外处理派发。** 将设置发布与 `fill_fingerprint_window_now()` 分开。补派会读取 Library 数据库，不能直接把整个 `apply_fingerprint_settings` 移入 app writer。让 worker 醒来后按最新暂停状态、豁免任务和容量决定是否补派。若保留延迟副作用，让它重新协调最新状态，防止迟到的暂停清理耗尽已恢复的队列。同步调整启动设置发布路径，沿用 `settings_revision` 防止旧快照覆盖新设置。

```rust
assert_eq!(saved["workers"], status["workers"]);
assert_eq!(saved["paused"], status["paused"]);
assert_eq!(status["workers"], 1);
assert_eq!(status["paused"], true);
```

- [ ] **验收。** 正反方向都测一次，包括「旧 resume 晚返回」和「旧 pause 晚返回」。降到 1 时已运行任务可以完成，后续普通任务要等活动数低于 1；另测契约允许的显式检测豁免。重建 ApiState 后读回同一设置，原有远程读取门限用例通过。

## 7 · P2　插件订阅者读取失败时保留待投递事件

**位置**　`crates/api/src/plugins/events.rs`，原子写入在 `crates/store/src/plugin_events/emit.rs`。测试沿用 `crates/api/tests/plugin_event_dispatcher.rs`。

- [ ] **先复现。** 安装有 `download.after` 双重授权的插件，在 `subscribe.db` 产生真实 outbox 事件。让本轮的 `app.db` 订阅者查询返回可恢复读错误，再调用 `drain_plugin_events_for_test`。通过 Store 的 `list_pending_fanout_events` 检查同一 `event_id` 仍待处理，恢复读取后再次 drain，并查询 delivery 记录。用限定于测试实例的读故障或受控 reader 占用；故障需覆盖订阅者查询，不能挡住前一步读取 outbox。
- [ ] **传播查询错误。** 将 `load_enabled_plugins_snapshot` 改为返回 `Result<Vec<EligibleSubscriber>, store::StoreError>`。查版本、授权、Site 时分别处理 `Ok` 与 `Err`，只缓存成功得到的值；授权成功读为空、Site 成功读为不存在或停用，仍按原规则筛选。持久化 manifest 解析失败需带插件身份记录，保留已有内置占位 manifest 的处理。
- [ ] **原子确定收件人。** 对一个事件，完整算出订阅者后才调用 `seed_event_fanout`。公共订阅者快照失败则结束本轮 fanout；某个事件的授权或 Site 读取失败则保留该事件为 `pending`。采用现有调度间隔重试，并允许已经 seeded 的其他事件继续投递。错误日志含数据库、事件或插件标识与错误类别。

```text
读取暂时失败     → 原 event_id 保持 pending，没有部分 delivery
读取恢复且有授权 → 同一 event_id 建立 delivery，已有去重键仍生效
读取成功且无人订阅 → seeded，零条 delivery
```

- [ ] **验收。** 覆盖订阅者列表失败和单个授权读取失败。两个授权插件中若后者读取失败，恢复后两者都有 delivery。工作器重启后也能接续处理。以 `(event_id, plugin_id)` 的持久记录核对去重；业务投递仍遵循至少一次语义。

## 8 · P2　将人工标记的文件观察移出 Tokio 请求线程

**位置**　`crates/api/src/http/marker_control/manual.rs`、`crates/api/src/fingerprint_job/source.rs`、`crates/api/src/blocking_budget.rs`。测试沿用人工标记 HTTP 用例。

- [ ] **先复现。** 在单线程 Tokio 测试中令 PUT 或 DELETE 的源文件观察停在屏障。由独立线程负责最终释放屏障，断言等待期间健康请求能先完成。可为源观察复用 `ApiState::with_file_stat_read` 的注入模式，也可用 `/tmp` 下的受控 `.strm` FIFO 重现阻塞。若采用严格墙钟断言，按 `AGENTS.md` 将时序测试放入独占测试二进制。
- [ ] **使用已有预算。** 将这两个入口的 `observe_source` 提取为共用异步调用，通过 `state.fs_work.run_to_completion` 在有界阻塞线程执行。先结束归属查询快照，再观察文件，最后进入 `store_write`。仍使用现有源版本比较和 Store 写入时的 `SourceObservation` 校验。

```text
store_read(归属与文件版本) → 返回并释放 reader
fs_work(观察文件，STRM 取内容版本) → 不可用或版本变化时返回原有 409
store_write(复核版本并更新) → 成功后通知章节截图服务
```

- [ ] **验收。** PUT、DELETE 各自通过阻塞观察测试。预算耗尽遵循现有 busy 响应；并发替换源文件返回 409，未写入过期标记；正常保存、解锁、删除及截图刷新继续通过。

## 9 · P2　让插件首次构建能取得锁定依赖

**位置**　`tools/plugin/src/build_cmd.rs`、`tools/plugin/src/scaffold_templates.rs`、`tools/plugin/tests/cli_workflow.rs`，CLI 行为记录在 `docs/plugins/authoring.md`。

- [ ] **先复现。** 用脚手架生成带 Rust component 的插件。隔离子进程的 Cargo 缓存，让一个锁定依赖仅可从测试控制的本地 registry 取得，执行 `plugin build`。断言能构建并打包 component，且 `Cargo.lock` 字节保持一致。普通测试不访问公网；需要时把真实空缓存镜像验收与 crate 的离线回归分开执行。
- [ ] **分开依赖锁定与离线选项。** 有 `Cargo.lock` 时给 `cargo build` 传 `--locked`，正常继承调用方的网络配置。显式离线构建使用 Cargo 已有的 `CARGO_NET_OFFLINE=true`。对无锁文件的受支持项目沿用现有解析方式。用真实脚手架确认生成的锁文件能直接通过 `--locked`；若 Cargo 要求更新，修正锁文件生成输入并重新验证。

```rust
if read_project_file_no_symlinks_optional(dir, "Cargo.lock")?.is_some() {
    cmd.arg("--locked");
}
```

- [ ] **验收。** 新缓存能构建；已预热缓存显式离线也能构建；缺少依赖时显式离线正确报错；清单与锁文件不一致时构建失败且没有改写锁文件。已有 `plugin test --offline` 和 `dev` 调用 build 的工作流均回归。

## 10 · P2　相对插件目录可以调用本地 Vite

**位置**　`tools/plugin/src/build_cmd.rs` 的 `resolve_local_vite`、`find_web_root` 和 `build_ui_bundle`，回归在 `tools/plugin/tests/cli_workflow.rs` 及已有符号链接测试中。

- [ ] **先复现。** 在临时父目录创建 `demo/ui`，提供能运行的本地 Vite，子进程以父目录为 cwd 调用 `plugin build ./demo`，再用 `plugin pack` 校验产物。旧代码会在 `demo/ui` 下再次拼接 `demo/ui/node_modules/...`。使用子进程 cwd，避免并行测试互改工作目录。
- [ ] **传递已检查的绝对路径。** 本地解析已计算 `canon_vite` 并检查它位于 `canon_ui` 内。将成功返回值由布尔值改为 `Option<PathBuf>`，把通过检查的绝对路径交给 Node。走宿主 Web 工具链时也先解析绝对 `web_root`，使 Vite 脚本和 `CM_WEB_ROOT` 在切换 cwd 后仍有效。

```rust
// 本地 Vite 路径经过既有类型和边界检查后的成功分支
Ok(Some(canon_vite))
```

- [ ] **验收。** 相对目录、绝对目录和目录名带空格三者都能产出带 `remoteEntry.js` 的有效包；已有越界符号链接用例仍被拒绝。单测使用仓库已安装的工具链或自包含夹具。

## 验证与提交

每项先跑相关过滤器，在测试输出里确认新用例确实被选中。Store、API、CLI 和 Web 的定向命令如下。参数中的 suite 名与当前仓库登记一致。

```sh
python3 scripts/with-build-lock.py cargo nextest run --build-jobs 4 --test-threads 4 -p api -E 'test(marker_standalone_control::)'
python3 scripts/with-build-lock.py cargo nextest run --build-jobs 4 --test-threads 4 -p api -E 'test(plugin_event_dispatcher::)'
python3 scripts/with-build-lock.py cargo nextest run --build-jobs 4 --test-threads 4 -p api -E 'binary(probe_settings_runtime)'
python3 scripts/with-build-lock.py cargo nextest run --build-jobs 4 --test-threads 4 -p store -E 'test(plugin_event_outbox::)'
python3 scripts/with-build-lock.py cargo nextest run --build-jobs 4 --test-threads 4 -p plugin-cli
python3 scripts/with-build-lock.py node --test web/test/marker-standalone-control.test.mjs
```

各改动 crate 跑完定向用例后，按仓库规则跑其 crate 套件。收尾跑 Web 检查、一次独占的 Rust workspace 套件和完整镜像验收。Rust、Web 和本地 registry 测试在允许回环监听的环境执行。

```sh
python3 scripts/with-build-lock.py cargo fmt --all -- --check
python3 scripts/with-build-lock.py cargo clippy -j4 --all-targets -- -D warnings
python3 scripts/with-build-lock.py pnpm --dir web lint
python3 scripts/with-build-lock.py pnpm --dir web typecheck
python3 scripts/with-build-lock.py pnpm --dir web test
python3 scripts/with-build-lock.py pnpm --dir web build
python3 scripts/with-build-lock.py --exclusive cargo nextest run --build-jobs 8 --test-threads 8 --workspace --no-fail-fast
python3 scripts/with-build-lock.py --exclusive docker build -t crawler-media:marker-review .
```

通过后在当前分支按独立修复提交。验收记录列出对应的红测、绿测和镜像检查结果；两项 P1 以成员权限测试和真实 Web 镜像构建均通过为准。
