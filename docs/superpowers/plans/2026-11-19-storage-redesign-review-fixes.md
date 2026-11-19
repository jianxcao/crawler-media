# feat/storage-redesign 七项审查修复计划

本计划针对 `feat/storage-redesign` 的 `83ed7506`，比较基点为 `756444f7`。目标是恢复默认启动、确保失败的 Library 修改可以完整回滚，并修复语言继承、播放凭据、CLI 分页和两处脚本行为。

实施时可使用 `superpowers:subagent-driven-development` 或 `superpowers:executing-plans` 逐项执行。依据为 `AGENTS.md`、`docs/adr/0008-sqlite-system-of-record.md` 和 `docs/api-contracts/self.md`。涉及 Rust、axum、rusqlite、SQLite 和本地 Shell 脚本。

先做第 1、2 项。第 3 项复用第 2 项的 Library PATCH 入口。其余四项可独立验证。每项先运行对应回归用例并记录失败，再实现并重跑同一用例。首次启动和脚本测试使用临时目录，避免触碰开发数据。

## 1. P1 · 未设置数据目录时也能启动

**现场证据**　在空目录启动 `crawler-media serve`，提供独立的 CLI token 和管理员密码，省略 `CRAWLER_MEDIA_DATA`，进程以状态码 1 退出，错误为 `root data/library/movies must be absolute`。失败发生在生成默认 Library 根目录时。

- [x] **先写进程测试。** 在 `crates/api/tests/relative_data_startup.rs` 用 `Command::current_dir` 指向临时目录，分别移除 `CRAWLER_MEDIA_DATA` 和设为 `nested/data`。通过 `/api/v1/health` 后读取 `/api/v1/libraries?scope=admin`，断言电影库、剧集库的根目录均为绝对路径，实际位于该临时实例中。停止后再启动一次，确认返回相同的 Library ID。每个子进程独立设置环境与工作目录。
- [x] **在 Store 打开文件时统一路径。** 修改 `crates/store/src/open.rs` 的 `open_files`。在创建目录、拼接各库路径和调用 `seed_defaults` 之前，分别对传入的 `data_dir` 和 `app_path` 调用 `std::path::absolute(...)` 并传播错误。`open` 已经拼好 `data_dir/app.db`，`open_at` 传入的相对 `sqlite_path` 仍相对调用进程工作目录解析。两个入口共享这处修复。
- [x] **验收路径语义。** 在 `crates/store/tests/storage_v1.rs` 覆盖 `open_at` 自定义 app 文件位置。外部提交相对 Library 根目录仍应收到 400；内部生成的默认根目录则应通过同一条严格校验。

## 2. P1 · 一个 Library PATCH 只产生一次提交

**现场证据**　`PATCH /api/v1/libraries/{id}` 提交 `{"name":"新名称","root_paths":["relative-test"]}`，响应是 `400 library.invalid_operation`。随后 GET 却已读到新名称。仅把路径检查提前，只能解决这个例子。另一个请求可能在检查完成后抢先取得重配置所有权。

- [x] **用 HTTP 和 Store 的可见结果写回归。** 在 `crates/api/tests/storage_api_contract.rs` 构造上述请求，同时修改 `admin_visible`。失败后用管理员的 `scope=admin` 读回原名称、可见性和根目录，确认没有新增操作。再用合法绝对路径请求移除仍有文件的唯一覆盖根，验证事务内校验失败也完整回滚。成功用例提交名称与合法新根，断言返回 202、可轮询同一操作、完成后两个字段均生效。
- [x] **锁外准备。** 在 `crates/api/src/http/library_config.rs` 将请求一次解析成普通字段补丁和可选的根目录／语言重配置。读取 Library 的 `config_revision`，通过 `LibraryReconfiguration::prepare()` 在 `fs_work` 中解析根目录。将准备结果、观察到的 revision 和普通字段一起交给 Store。
- [x] **在一个 `library.db` 事务内受理。** 重点改动 `crates/store/src/libraries/connection.rs`、`crates/store/src/library_operations/connection.rs` 和 `crates/api/src/storage_operations.rs`。取得唯一 Library writer，在同一连接的外层 `write_tx` 中复查 revision、生命周期、删除屏障、冲突操作及文件覆盖；随后写名称、权限、开关等普通字段。需要重配置时，一起插入操作、设置 `reindexing` 并推进 revision，最后只提交一次。`write_tx` 已支持借用同一连接上的事务。提取现有 SQL 步骤供两条路径复用，避免在事务内再次申请 writer，或提前运行现有 helper 的提交后动作。
- [x] **提交成功后才发布副作用。** `crates/api/src/library_lifecycle.rs` 的 `set_optional_settings` 目前会单独写开关并取消工作。将开关写入合并到上面的事务，按既有 Registry → Store 顺序协调新任务准入。成功提交后更新内存屏障、收集待取消任务，释放锁后再执行取消、启动 driver。根目录别名刷新和批处理也留在提交后。事务失败应让已有工作继续运行。
- [x] **验证并发与恢复。** 在 `crates/api/tests/storage_operations_runtime.rs` 验证竞争者先取得所有权时，失败请求返回 409，且其名称和开关均未写入；持久化操作在重启后继续。普通字段请求返回 200。将这些响应语义写入 `docs/api-contracts/self.md`。

## 3. P2 · 让 Library 真正继承实例语言

`libraries.display_locale` 的 DDL 默认值为 `zh-CN`，默认库和 API 创建库均省略该列。查询优先取非空的单库值，因此把实例语言切换到 `en-US` 后，重建成功也仍会优先选择中文。现有 `query_model_locale` 测试手动用 SQL 清空该列才构造出继承场景。

- [x] **通过正常创建入口造数据。** 在 `crates/store/tests/query_model_locale.rs` 创建两个都拥有中英文标题的 Library。第一个沿用初始配置，第二个通过重配置显式设为 `zh-CN`。实例改为 `en-US` 并推进操作至成功后，通过浏览查询读回英文和中文。再把第二个库切回继承并读回英文。额外覆盖实例语言变更后新建的库。用这组行为断言替换手工 `UPDATE libraries`。
- [x] **明确“继承”和“本次不改”。** 建议沿用 Store 已识别的空串作为持久继承值，在 `crates/store/src/schema/v1/library.sql` 将 `display_locale` 默认设为 `''`。PATCH 缺少 `display_locale` 表示保留原配置，传 `{"display_locale":""}` 表示清除覆盖，非空字符串表示显式覆盖。修改 `crates/api/src/http/library_config.rs`、`crates/store/src/library_operations/preparation.rs` 和 `locale.rs`，让显式空串进入持久化重配置和卡片重建。
- [x] **对齐展示和布局版本。** 校验卡片、详情和协议展示按“单库覆盖 → 实例默认”选择语言；在实例也未配置时保留约定的默认展示行为，并用中英文同时存在的条目验证。更新 `docs/api-contracts/self.md` 和 `docs/database/library.md`。采用上述 DDL 方案时同步更新 `crates/store/src/schema/layout.rs` 当前 Library 布局版本及 `docs/deploy.md`。本仓库逐项校验 DDL，现有 v6 数据目录会被新版拒绝打开，验收使用独立新目录，保留原目录和一致备份。

## 4. P2 · 播放 URL 的凭据只解码一次

`playback_query` 已对 token 进行表单编码，例如 `ab+c/==` 的 `+` 会写成 `%2B`。`crates/api/src/http/auth.rs` 的 `query_token` 当前直接把查询串中的原始值送去查会话。

- [x] **测试真实签发的 URL。** 扩展 `crates/api/tests/playback_read_regression/stream_credentials.rs`，为测试用户配置含 `+`、`/`、`=` 的有效 token，以 Bearer 创建会话，再原样使用响应的 `stream_url`，只带 `Range: bytes=0-0` 发起 GET，断言 206、响应体为预期的一个字节。用同一有效凭据覆盖有字幕的签发路径。
- [x] **对称解码。** `query_token` 保留现有路由白名单，使用 `url::form_urlencoded::parse(query.as_bytes())` 迭代已解码的键值，按现有大小写规则匹配 `api_key`、`apikey`、`token`，返回 `value.into_owned()`。鉴权顺序继续使用 Bearer、允许的 query token、cookie。
- [x] **验收凭据边界。** 测试带其他用户 cookie 的合法播放 URL 仍能读取原会话；将同一 query token 用于普通成员路由仍返回 401。失败断言和日志中仅打印状态码及路由名。

## 5. P2 · CLI 读取 Library 的所有游标页

`GET /libraries/{id}/items` 现在默认每页 500 条。响应的 `next_cursor` 与 `data` 同层。`HttpTransport::send` 和测试的 `RouterTransport` 都会丢弃 `data` 外面的字段，所以仅在 `list_library` 外面加循环拿不到下一页。

- [x] **通过 CLI 输出验证完整性。** 在 `crates/api/tests/management/cli_library.rs` 为一个 Library 准备 501 个可区分的标题，运行 `Command::Library`，断言 501 个标题逐个恰好出现一次。另用两页响应覆盖含 `+`、`&`、`%` 的不透明游标，以及第二页返回错误的情况。后一种情况应返回 CLI 错误。
- [x] **保留完整信封。** 在 `crates/api/src/cli/mod.rs` 与 `crates/api/tests/management/cli.rs` 将现有底层请求提取为 `send_envelope`，返回 `(status, 完整 JSON)`。现有普通 `send` 在其上执行原有 `unwrap_data`。让分页读取显式使用完整信封，避免对其他 CLI 命令的返回值做重复解包。
- [x] **逐库读取。** 修改 `crates/api/src/cli/lists.rs`。每个 Library 从无游标的第一页开始，输出 `data` 中的每个标题。`next_cursor` 为字符串时用 `url::form_urlencoded::Serializer` 构造下一次查询，原值只编码一次；为 `null` 时结束当前库。遇到非成功响应、畸形信封或重复游标则返回错误。保留空库的 `No Media` 输出。

## 6. P2 · 本地启动沿用单实例租约

- [x] **在脚本边界写回归。** 扩展 `scripts/test_start_test.py` 的假后端，记录它收到的 `CRAWLER_MEDIA_ALLOW_MULTI_INSTANCE`。启动测试先隔离该变量，断言默认未设置，再分别传入 `0` 和 `1`，断言子进程收到相应值。
- [x] **删除强制覆盖。** 从 `start-test.sh` 的 `launch backend env` 参数中移除 `CRAWLER_MEDIA_ALLOW_MULTI_INSTANCE="1"`。`env` 已经继承调用者的导出变量，后端 `instance_lease::acquire` 负责应用租约策略。
- [x] **按进程行为验收。** 使用临时数据目录和两个不同监听端口验证：默认配置下第一实例持有写租约，第二实例的 `/api/v1/health` 显示 `read_only: true`。用不同端口可以避免脚本的端口清理掩盖结果。

## 7. P2 · 压测启动脚本遵守共享构建锁

- [x] **写隔离的脚本测试。** 在 `scripts/e2e/test_stress_up_build.py` 建立临时脚本树，复制实际 `stress-up.sh` 和 `with-build-lock.py`，在临时 `PATH` 放置假 `cargo`。测试使用独立 `CM_BUILD_LOCK_DIR`。先占住一个构建槽，再以默认参数启动脚本。使用独立包装器的等待行为和假 `cargo` 的启动记录，验证独占构建要等已有槽释放；假 `cargo` 记录参数后以受控错误结束，测试随即停止，不启动服务或生成媒体。
- [x] **统一默认构建入口。** 将 `scripts/e2e/stress-up.sh` 中直接调用 Cargo 的一行替换为与 `docs/testing/performance-slo.md` 一致的构建方式。

  ```sh
  python3 "$P/scripts/with-build-lock.py" --exclusive \
    cargo build --manifest-path "$P/Cargo.toml" --release -j8 -p api
  ```

- [x] **覆盖跳过构建的两条路径。** 在 `restart` 和 `PERF_SKIP_BUILD=1` 场景，验证脚本越过构建阶段。这样外层已持有独占锁的正式冷启动测量可以执行 restart。测试使用临时 pid 和替身命令在下一阶段结束，全程避开真实开发实例。

## 定向测试与收尾

以下命令用于实施阶段。每项先以新增测试名过滤得到 red，再运行相应测试目标确认 green。Shell 测试用临时锁目录，避免与外层包装器竞争同一组锁。

```sh
python3 scripts/with-build-lock.py cargo test -j4 -p api --test relative_data_startup -- --test-threads=4
python3 scripts/with-build-lock.py cargo test -j4 -p store --test storage_v1 -- --test-threads=4
python3 scripts/with-build-lock.py cargo test -j4 -p api --test storage_api_contract --test storage_operations_runtime -- --test-threads=4
python3 scripts/with-build-lock.py cargo test -j4 -p store --test query_model_locale -- --test-threads=4
python3 scripts/with-build-lock.py cargo test -j4 -p api --test playback_read_regression stream_credentials -- --test-threads=4
python3 scripts/with-build-lock.py cargo test -j4 -p api --test management cli_library -- --test-threads=4
python3 scripts/with-build-lock.py python3 -m unittest discover -s scripts -p 'test_start_test.py'
python3 scripts/with-build-lock.py python3 -m unittest discover -s scripts/e2e -p 'test_stress_up_build.py'
```

七项合并验证时依次运行 `cargo fmt --all -- --check`、改动 crate 的测试，然后用一次独占 workspace 测试收尾。

```sh
python3 scripts/with-build-lock.py cargo fmt --all -- --check
python3 scripts/with-build-lock.py cargo test -j4 -p store -- --test-threads=4
python3 scripts/with-build-lock.py cargo test -j4 -p api -- --test-threads=4
python3 scripts/with-build-lock.py --exclusive cargo test -j8 --workspace --no-fail-fast -- --test-threads=8
```

## 执行记录（2026-11-19）

七项实现与回归用例均已完成。启动进程用例实际放在 `crates/api/tests/relative_data_startup.rs`；Library PATCH 的确定性并发用例放在 `crates/api/tests/storage_api_contract/library_patch.rs`，在路由准入之后、实际 writer 事务之前插入竞争操作，覆盖普通设置与重配置两条路径。Store 另覆盖旧准备结果在竞争操作完成后仍因 revision 变化而回滚，以及提交后的操作重开续跑。

已通过的定向验收：启动与 HTTP 合约 14 项、Store 布局／原子重配置／语言投影 18 项、可选探测开关 7 项、操作运行时 17 项、播放读取 15 项、CLI 分组 56 项；启动脚本 12 项、压测脚本 3 项。`cargo fmt --all -- --check`、`cargo clippy -j4 -p store -p api --all-targets -- -D warnings`、Shell 语法、改动 Rust 尺寸与 `git diff --check` 均已通过。

首次独占 workspace 验证在 API 单元测试 316 项通过后中断：运行期间执行了 `cargo clean`（用户已确认），`target/` 被清除，后续测试可执行文件无法启动。该轮不计为完整通过，随后按相同独占命令冷构建并恢复全量验收。

恢复轮完整执行了 645 个目标（含 doc-tests）：4,492 项通过、1 项失败、28 项按原配置忽略，命令退出码 101。唯一失败为 `library_reconfiguration_roots::roots_and_locale_commit_together`：旧夹具仍将空白 `display_locale` 当作非法输入，与本计划第 3 项的继承契约冲突。仅将其拒绝场景改为确实非法的空根目录和有效语言，保留“拒绝后不冻结”和“有效根目录与语言共同提交”的行为断言，未追加修改生产代码。

依资源约定不重复整个 workspace，随后定向重跑该目标：

```sh
python3 scripts/with-build-lock.py cargo test -j4 -p store --test library_reconfiguration_roots -- --test-threads=4
```

重跑结果为 **5/5 通过，退出码 0**，已无待处理的测试失败。9,500 条 Media 字母跳转、4,000 条分页与查询工作量增长用例，以及本次新增的并发 PATCH、语言继承、真实进程启动／租约和播放／CLI 回归均已执行通过。28 项忽略用例保留仓库原有标记（含手工性能测量与外部环境验收），未修改启用条件，未计入通过数。

修正旧夹具后，收尾再次运行 `cargo fmt --all -- --check` 及 Store/API 的 `cargo clippy -j4 -p store -p api --all-targets -- -D warnings`，均退出 0；`git diff --check` 和改动 Rust 文件 800 行硬限制检查通过。所有 Cargo 命令均由共享构建锁包装器执行。
