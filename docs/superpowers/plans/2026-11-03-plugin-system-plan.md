# 插件系统实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
> 状态：全部待实施。本轮只交付设计和审查；复选框不表示测试已运行。新文件/接口均为待创建目标，现存落点以 `52522549` 为基线。

**Goal:** 交付可编写、安装、授权、运行和诊断的插件系统，完整覆盖自定义 Site、PT 搜索与 Subscribe 中间干预、CDP 搜索/签到、刷流闭环。

**Architecture:** WASM Component 的同步隔离运行时通过类型化 port 调宿主；业务写走持久 command，读写复用现有 Store/Jobs 边界，结果经生命周期与版本校验后发布。第一方插件和第三方组件使用同一能力合同。

**Tech Stack:** Rust 2024、SQLite/现有 ReaderPool 与 Writers、Wasmtime/WIT（P0验证并精确锁版本）、现有 Browser/CDP、React/Vite/TS/pnpm。

**Spec:** [系统设计](../specs/2026-11-03-plugin-system-design.md)、[作者接口](../../plugins/authoring.md)、[界面/API](../../plugins/ui-and-api.md)、[ADR-0013](../../adr/0013-wasm-plugin-runtime.md)。执行者必须同时读取四份合同，不从旧 r6 推导行为。

## Global Constraints

- 零兼容：无旧Bus/Downloader默认转调、双MAJOR、旧字段fallback、自动DB/KV迁移。
- 读不取writer；实际writer不得嵌套；数据库/注册表锁和reader lease不跨guest、网络、文件IO或await。
- app/subscribe/library/cache读池4/2/8/2；Jobs读池2；前台writer等待1s。远程probe并发1、冷却500ms不可改。
- 跨库阶段独立提交，有稳定operation/generation与恢复意图；外部副作用Unknown先对账，不盲重试。
- Library删除按ADR-0012先隐藏取消，迟到结果不得发布，真实删文件使用当前持久删除证据。
- 所有测试先red再green，断言public seam可观察结果，不断言私有helper、SQL字符串、mock调用次数。cargo测试不上外网、不启动Chromium。
- 同时只有一个cargo/test/build，持有`/tmp/cm-cargo.lock`，设置`TMPDIR=/tmp`；cargo `-j8`，测试`-- --test-threads=8`。全量只收尾一次`--no-fail-fast`。
- 只提交当前实施分支、相关文件；不推main、不force push、不擅自开PR/issue；测试报告和`.e2e/`不提交。

## 依赖与实施顺序

```text
P0 协议/最小过滤组件 -> P1 存储/命令/lifecycle -> P2 Site+HTTP+Browser+签到
                                             -> P3 下载统一/全流程Hook
P2 + P3 -> P4 刷流闭环 -> P5 Transfer/Subscribe恢复/可靠事件
P1 + P2 + P3 -> P6 作者工具与完整管理UI
P1 + P3 + P5 -> P7 榜单/媒体/通知/endpoint/widget
全部 -> P8 文档/安全/规模/系统验收
```

可独立修改的端口/页面允许并行，任何build/test串行；每个切片先做fake驱动的完整用户路径，再接真实适配器。P0–P4均为核心交付，不能把CDP或刷流移到“以后”却宣称插件系统完成。P6页面随相应后端切片同步交付，不等所有后端完成才验证用户流程。

## 公共接口地图

类型的字段与错误闭集以作者文档为准，WIT `.wit` 是实施后的唯一代码定义；以下是跨切片端口责任，不是可直接复制编译的旧样例。

| 端口 | 生产者 → 消费者 | 输入/结果 | 责任 |
|---|---|---|---|
| `PluginInvoker::invoke` | plugin-host → hooks | Context + typed HookInput → typed HookOutcome | 预算、权限、generation，不能接原始Store |
| `HostServices` | api → plugin-host | read/http/browser/command capability对象 | 不使plugin-host依赖api |
| `PluginCommandService::submit` | api → import/HTTP | subject + kind + key + payload → operation_id | 幂等接受；不等待业务完成 |
| `IndexerProvider` | indexer port；native/WASM实现 | SiteContext + query/ref → page/session/status | typed provider能力；无直接凭据 |
| `MaintenanceProvider` | hooks/indexer共享窄port | SiteContext + action window → result | Check-in和verify-session，不隐式搜索 |
| `BrowserSession` | indexer/宿主 → import | scoped actions/resources → bounded result | 单context、真实egress、取消清理 |
| `DownloadSubmitter` | downloader port；api实现 | frozen plan + context + operation_id → Submitted/Rejected/Deferred/Unknown | 四条业务入口和插件写共享 |
| `TransferDecision` | hooks →各Transfer入口 | candidate + observed revision → relative path/decision | reserve前；实际发布仍用原WriteScope/FileVersion |
| `EventDispatcher` | api → Notify | stable event_id + typed committed event | outbox至少一次，去重/保序/死信 |

## P0：精确协议与最小可用插件

文件：新增`crates/plugin-api/wit/`、`crates/plugin-host/src/{engine,invoke,limits,ports}.rs`、`crates/hooks/src/{pipeline,payload,decision}.rs`；修改Cargo.toml/锁文件、hooks/lib.rs；新增`crates/plugin-host/tests/{policy_component,budgets}.rs`与fixtures。SDK独立workspace位于`sdk/`，不把示例guest依赖偶然并入宿主。

- [ ] 先将作者文档WIT摘录变成唯一真实包，定义全部world/import/export、typed errors、context只读视图；生成host/guest绑定并实现最小`search.after`组件。
- [ ] 写公开调用测试：候选A/B传入组件，A被drop，B仍保留原opaque handle；未知handle patch被拒且返回原输入。错误variant须区分trap与guest业务错误。
- [ ] 运行`TMPDIR=/tmp cargo test -j8 -p plugin-host --test policy_component -- --test-threads=8`，记录red；添加最小invoke实现后同命令green。
- [ ] 测试缺失/未知import、精确world不匹配、未授权import、死循环/内存超限、等待预算到期、真实worker未退出不释放容量。将epoch用作墙钟，fuel用作指令上限；不写CPU毫秒保证。
- [ ] 用当前工作区可用工具链验证受支持Wasmtime/wit-bindgen组合、wasm目标、macOS/Linux构建；精确锁版本并保存构建命令。依赖安装仅解决该切片必要内容；不沿用“rustc1.85最高31”的历史断言。
- [ ] 列出所有现有Bus调用者，逐批改为typed port和同义第一方行为，删除旧API；原`hooks/tests/login_checkin.rs`、`review_remediation_login.rs`仍验证现有认证行为。
- [ ] 通过审查后提交该切片，记录真实限制（无网络/无业务写状态也能完成过滤demo）。

验收：一个真实组件改变PT候选而不能伪造来源；无插件时宿主行为保持；没有第二套文档WIT长期漂移。

## P1：插件登记、授权、持久命令与生命周期

文件：新增`store/src/plugins/`、`api/src/plugins/{service,commands,reconcile,lifecycle}.rs`；修改`store/src/schema/v1/app.sql`、`schema/layout.rs`、`jobs/src/{kinds,schema.sql,types}.rs`、`api/src/{jobs_api,job_loop}.rs`；新增`store/tests/plugin_operations.rs`、`api/tests/plugin_lifecycle.rs`。

- [ ] 按设计§7落当前DDL、布局验证与数据字典，不建plugin.db；初始化/重开/额外对象/旧布局拒绝均有public Store测试。
- [ ] Red：同subject/key/body只返回一个operation；同key异body冲突；两个并发KV写不能越quota；取消或deadline后lease不早归还。
- [ ] 实现app operation接受与目标业务幂等键；Jobs投影独立提交，稳定operation/generation/revision防重复，按失败点逐一恢复。
- [ ] Red：接受command后撤权→执行被拒；旧generation command在重新启用后仍不运行；重复cause pair和深度>4不会跳过安全Gate继续执行。
- [ ] 实现invoke/input/output校验、grants与subject交集，业务write只T/E返回operation；F/G/N写入返回wrong-context。fake外部副作用在guest结束后由独立worker运行。
- [ ] Red：凭据域A写入KV/blob/operation/输出，B域及global调用都不能读取；人工换凭据生成新域，正常同域session刷新不换。实现宿主security_scope贯穿全部读取/持久状态/输出，标准候选释放边界与Provider信任限制在授权UI可见。
- [ ] Red：install在解包/DB登记/active指针各崩溃点可恢复；disable在途返回draining；旧实例卡住不显示disabled；重启先应用禁用意图。
- [ ] 实现签名/包预检、prepare-consent、配置revision、state_schema一致升级与不一致明确reset、卸载保留业务事实。文件IO与编译全在writer外。
- [ ] 定向运行`cargo test -j8 -p store --test plugin_operations -- --test-threads=8`与`-p api --test plugin_lifecycle`（均TMPDIR=/tmp且同锁）；green后review/commit。

验收：可离线安装一个默认停用的签名包，授权/配置/启停的持久状态可查询；不确定业务结果不被标成未执行。

## P2：Site Provider、HTTP/CDP与签到插件

文件：修改`indexer/src/{engine,profile,fetch,browser,browser_host}.rs`、`api/src/{site_maintenance,check_in}.rs`、`http/sites/{mod,maintenance}.rs`及实际Site模块；新增`indexer/src/provider.rs`、`plugin-host/src/imports/{http,browser}.rs`、`crates/net-guard/`、`api/src/plugins/maintenance.rs`、`examples/plugins/{custom-site,checkin}/`。测试新增`indexer/tests/plugin_sites.rs`、`net-guard/tests/policy.rs`、`api/tests/plugin_checkin.rs`。

- [ ] Red：RegistrySnapshot切换后单次搜索全用同一profiles/providers代；缺失provider得到明确SiteFailure；自定义Site能力不是Framework分支猜测。
- [ ] 实现原子registry，profile返回Arc，批量更新所有调用者/goldens；声明式包ID命名空间化，冲突拒绝。
- [ ] Red：实际连接IP必须属于校验集合；DNS rebinding、特殊地址、跨域redirect、代理未验证、种子URL fallback皆被拒；fake请求结果可观察，不靠mock调用次数。
- [ ] 实现net-guard，保持站点限速；CredentialSlot仅宿主注入、响应脱敏；每包目的地授权与组件权限交集，插件不能回调本实例绕过commands。
- [ ] Red：Provider→HTTP→site.request/response Hook在同插件并发1时仍可结束，子Hook再调HTTP/Browser/DB/KV均wrong-context；实现深度1纯内联子Store、父子内存合计和非阻塞permit，自动Browser子资源仅静态net-guard。
- [ ] Red：Browser总开关/Site用途/grant任一缺失即无会话；第三方会话必须证明egress隔离，普通远端CDP返回browser-isolation-unavailable。
- [ ] 扩展PageSession为窄scoped action port；受管Browser网络代理固定校验目的IP，禁止直连、子frame/worker/WebSocket绕过；会话cleanup/取消与槽位真实释放有fake测试。
- [ ] Red：自定义Site经Browser搜索得到Torrent；未知下载ref不能跨Site用；Browser点击签到后timeout得到unknown并在同窗口不重POST；Cookie人工更新与回写冲突不覆盖。
- [ ] 第一方NexusPHP Maintenance Provider封装现有成功/已签到/挑战判定；Check-in task按Site/window记receipt，HTTP/Browser分别可验收；自定义API站点可实现check-in能力。
- [ ] Red：两个维护插件争抢同Site/action/window只接受一个动作；更换Provider/凭据、重启或时区变化不能重做成功/unknown窗口。唯一receipt不含plugin_id。
- [ ] 删除check_in.rs内boost_check耦合；旧签到入口如保留业务URL应直接调同一服务，不保留旧实现fallback；现有HTTP/Browser回退只有导航前可用，动作发出后不可再试。
- [ ] 先fake suites green，再单独受控Browser E2E验证真正IP/子请求隔离和关闭清理；通过前不开放第三方Browser安装启用。
- [ ] 同步交付Site配置Browser授权、插件签到任务详情和每Site结果UI，review/commit。

验收：自定义Site支持HTTP和获授权CDP搜索；签到是真实插件契约，停用停止新动作；单测不访问真实PT、不启动Chromium。

## P3：下载统一与搜索/Subscribe干预

文件：新增`downloader/src/submit.rs`、`api/src/plugins/download_service.rs`；修改`downloader/src/{client,qbit,transmission}`实际分模块、`api/src/{delivery,admit}.rs`、`admit/delivery.rs`、`http/downloaders/submission.rs`、`worker/finish_search.rs`、`management/subscribes/run/admission.rs`、`subscribe/src/{add,types}.rs`；subscribe DDL增加下载operations/ownership/defer；新增`api/tests/plugin_download.rs`、`subscribe/tests/plugin_decisions.rs`。

- [ ] 先逐入口写行为测试：定时/手动Subscribe、普通手动、候选admit、CLI与plugincommand均受同一download.before结果约束；reject后Downloader中无任务、defer有持久原因，别用hook调用计数替代结果。
- [ ] 实现DownloadSubmitter同步port，async外壳沿http_work；一次性替换旧add签名，保留具体Downloader选择、stable enclosure、pending与ExistingTaskBroken。
- [ ] Red：同一operation响应丢失后为Unknown，重启不重复add；已有foreign同hash不会变owned；Transmission无labels但有hash可证明，有效证据缺失不能删。
- [ ] 实现submitted/unknown对账与容量reservation，不因10分钟查不到就删除可能有副作用的意图；明确查询成功且否定后才释放。
- [ ] Red：搜索前改关键词、单站过滤、汇总过滤、Filter后重排/score、select删减都改变公开结果；filter.after不能复活拒绝项，选择不能注入未知handle。
- [ ] 把系统设计§5 Hook目录注册成单一typed catalog；为每个Hook定义字段白名单/上下文/错误策略，管理UI与SDK从它生成目录。绑定Site/auth/session hooks，不仅订阅add处。
- [ ] Red：plugin patch改Downloader时冻结新实例，后续默认配置改变不漂移；Library删除/Subscribe取消后迟到add响应只能对账，不重新发布pending。
- [ ] 实现插件来源torrent bytes提交与net-guard；qB multipart和Transmission metainfo有fake协议端点测试，URL fallback不可存在。
- [ ] Red：Provider size=1而真实metainfo总量1TB，按真实长度拒绝或最终预留；元信息总长度溢出拒绝，unknown-size magnet不自动刷流。最终reservation绑定宿主解析的verified_total_bytes/hash/bytes摘要，之后才add。
- [ ] 定向api/subscribe/downloader/indexer测试green，review/commit；显示各插件影响轨迹、被拒/延后的公开业务结果。

验收：PT搜索可过滤、重评分、优先排序且宿主准入仍有效；所有下载入口受同一决策合同，结果不确定可恢复。

## P4：第一方刷流插件

文件：新增`examples/plugins/boost/`（可先用同合同native实现再交付WASM示例）、`api/src/plugins/boost.rs`、`store/src/download_operations/`、subscribe DDL reservation/task facts；删除`api/src/http/sites/boost.rs`旧假执行路由与web旧boost.*配置入口；新增`api/tests/plugin_boost.rs`、`web/test/plugin-boost.test.mjs`。

- [ ] Red：初始enabled但无Job回执UI不显示运行；磁盘budget不会拿uploaded_bytes比较；Downloader失败不会读成0并继续添加。
- [ ] 写两个插件轮次争抢最后容量的公开用例：总reservation+active+retained不能超budget；Unknown占额，确认失败才释放。
- [ ] 实现发现→共用search/filter→计划→容量预留→command add→下轮对账；专用category是组织字段，ownership才是授权证据。
- [ ] 实现guest `boost.snapshot`和`downloader.prepare-cleanup`：前者读取权威reservation/retained与有时效磁盘证据，后者持久化10分钟有效、绑定身份版本的计划handle；guest不回调Admin REST，REST复用同服务。
- [ ] Red：HR未知/未到hold_days/其他任务文件引用均阻止清理；delete-task后残留文件仍占quota；foreign同hash、手工改tag、变更路径不能获得删文件权。
- [ ] 实现host保护检查和持久文件删除计划；Downloader无法证明文件安全时返回unsupported并显示人工清理，不能直接传delete-files=true。
- [ ] 增加dry-run、每Site/Downloader归属统计、限速、每小时添加数和最小剩余磁盘；重启恢复未完成轮次/游标，统计陈旧显示时间。
- [ ] public tests覆盖预算竞争、远程超时、任务先删/文件未删、跨Site统计、停用与保护；green后review/commit。

验收：可实际自动选择和投递候选、保护做种、清理并准确释放容量；UI每个状态有receipt来源。

## P5：Transfer/Subscribe持久生命周期与事件

文件：修改`subscribe/src/collect/transfer.rs`、`api/src/worker/transfer/{plan,decision,recorder,publication,removals}.rs`、`watch_intake.rs`、`claim/publication.rs`、`subscribe_create.rs`、`completion.rs`、`http/subscriptions/deletion.rs`、`library_lifecycle/removal.rs`；新增`store/src/plugin_events/`、`api/src/plugins/events.rs`及所属库outbox DDL；新增`api/tests/plugin_lifecycle_races.rs`。

- [ ] Red：transfer.rename修改目标后最终reserve对应新目标；插件返回root/file/version被拒；旧目标不能残留错误reservation。
- [ ] 在reserve前接候选Hook，保留现有磁盘观察和发布CAS；Watch/claim也消费共用port，不改其Move先Copy再发布/删源次序。
- [ ] Red：删除Library时插件暂停，删除完成后放行插件仍无法发布；Subscribe revision变更使completion-check旧决定失效。
- [ ] Subscribe create/update/delete沿现有creation generation、retention、Jobs同步与补偿；在每个提交边界注入崩溃并重开验证不会重复Gate或复活新代。
- [ ] Red：事务回滚无event；commit后崩溃恢复仍可投递；重复event_id插件去重；同实体顺序不乱；跨库不要求假全局顺序。
- [ ] 实现所属库outbox与delivery receipts、cursor/dead-letter；Notify只读，不借Store层调插件；Library删除/真实删文件事件与“意图已接受”分开。
- [ ] 真实文件删除重复/路径复用/旧deletion_id测试green；保留file版本、favorite/progress孤儿清理回归；review/commit。

验收：插件干预不破坏现有持久恢复和删除语义；成功事件确实描述已提交事实。

## P6：作者工具与完整管理界面

文件：新增`sdk/crawler-plugin-sdk/`、`tools/plugin/`、`examples/plugins/{policy,custom-site,checkin,boost}/`；新增`api/src/http/plugins/`、web的`lib/api/plugins.ts`、`components/plugins/`、`app/(app)/settings/plugins/[id]/page.tsx`；修改`web/src/main.tsx`、settings sections、permissions及相关测试。

- [ ] Red：包缺文件、未知manifest字段、越路径/符号链接/压缩炸弹/签名不对/同版本换hash全被拒；schema不兼容不会悄悄迁移；SDK最小example生成组件可装入宿主。
- [ ] Red：YAML-only有效签名包无需WASM可安装；空包拒绝；正常模式离线上传cmplugin+detached minisig成功，未签名正常模式拒绝。示例ID都符合反向域名规则。
- [ ] CLI完整实现作者文档的scaffold→validate→test→build→pack→sign→dev命令；正式安装使用管理UI/API，本文不新增未定义的CLI install。dev也必须cmplugin与权限，无裸wasm逃逸；示例仓库不包含秘密。
- [ ] 按UI文档API逐表实现，包含prepare/confirm、操作轮询、grants、任务schedule/run、config validate、审计、hook catalog、scoped endpoint operation查询。
- [ ] Red：无repo也能导入经指纹确认的公钥并安装签名包；撤销key后拒新激活。state_schema变化没有明确reset/完整target_config不能升级，确认token不覆盖改变后的reset范围；state导出仅原Admin重新鉴权下载，过期失效，不出现在审计/普通operation JSON。
- [ ] Red：Member直达管理pathname与?tab=lists都不可见且API403；endpoint user/apikey不能读取Admin操作；CSRF/身份/reauth token按当前认证合同处理。
- [ ] 实现JSON Schema表单、secret keep/replace/clear、CAS409草稿保留、428缺revision、Idempotency-Key、202操作状态；超时保存key查operation不盲发新请求。
- [ ] UI准确展示未配置/运行/熔断/draining/unknown/Browser不具隔离；搜索轨迹只给有权用户最小原因，Admin诊断才展示更完整数据。
- [ ] 定向`pnpm --dir web test`筛选对应公开逻辑测试（以package实际命令传参），再lint/typecheck/build串行；Browser视觉E2E只跑本切片必要页面，review/commit。

验收：新作者无需阅读宿主内部可写四类示例；管理员可完成安装到卸载全流程，非Admin不能绕过UI/API边界。

## P7：榜单、媒体、通知与声明式集成

文件：新增`api/src/plugins/{lists,notifications,endpoints,widgets}.rs`、subscribe DDL的list_*；修改`http/discover/`、通知路由、相关Library/Playback事件入口；新增`web/components/plugins/list-subscriptions-view.tsx`和榜单路由；测试新增`api/tests/plugin_integrations.rs`。

- [ ] Red：ListSync分页受限、aliases kind隔离、重复榜单不重复创建Subscribe、用户删除后ignored意图阻止再建；suggest不会触发下载。
- [ ] 实现ListProvider/ListSync，身份不确定进入待确认；auto_subscribe走相同服务与所有Gate，不写Store捷径。
- [ ] Red：Notify channel递归发送被拒；endpoint HTML/Set-Cookie/外站Location被拒；apikey仅header，跨subject operation查询拒绝。
- [ ] Red：API key只操作显式capabilities/object_scope，签发者停用或降权后执行拒绝；同一issuer的另一key也不能读原key operation，轮换/撤销后旧key失效。持久key主体按key_id+issuer构造，不能退化为system任务身份。
- [ ] 实现渠道/endpoint/widget固定schema；图片代理签名绑定plugin/generation，撤权不可继续拉图，原public静态图片路由保持public。
- [ ] Red：媒体建议不能覆盖既有冲突alias、未opt-in用户没有Playback事件，用户撤销后下一次投递受阻；progress有界合并。
- [ ] 实现media识别建议/库刷新命令与用户隐私授权；不要把catalog cache当已采用Media事实；green后review/commit。

验收：榜单到Subscribe、通知渠道与Webhook都按既定主体/恢复合同运行；不执行插件网页代码。

## P8：收尾验收与发布门槛

- [ ] 从完整Hook目录生成覆盖矩阵：每项输入、允许patch、拒绝、取消、失败、事件时点都有public行为测试；未实现项不在运行时catalog宣称可用。
- [ ] 锁内guest/网络/文件IO、二次writer、跨库假事务、无界列表、秘密日志逐项review；所有Important/Critical问题关闭后才验收。
- [ ] 规模验收按现有SLO记录版本/数据量/资源：纯Hook目标、池饱和、Browser占用、Plugin KV审计保留、刷流并发quota、Library删除/Playback延迟。目标不是历史通过证据。
- [ ] 受控E2E验证CDP所有子请求真实出站/取消/清理、qB/Transmission真实协议；单元测试仍完全offline。验收未通过的能力不可被开启。
- [ ] 持有同一cargo锁，依次运行下面命令；仅最终全量一次，无失败时不重复扩大测试。若发现失败先查根因，不提高timeout或远程probe并发。

```sh
cargo fmt --all -- --check
TMPDIR=/tmp cargo clippy -j8 --all-targets -- -D warnings
TMPDIR=/tmp cargo test -j8 --workspace --no-fail-fast -- --test-threads=8
pnpm --dir web lint
pnpm --dir web typecheck
pnpm --dir web test
pnpm --dir web build
```

- [ ] 更新CONTEXT、ADR-0002/0007、当前API合同和数据库字典，只有实际实现能力改成当前时态；本提议ADR按实际验收决定是否接受。
- [ ] 提交当前实施分支，交付示例包、文档、测量与已知限制；部署涉及旧布局重建仍须单独核实实例路径/关闭连接/授权，不让启动自动删库。

## 设计审查与本轮边界

本计划是后续实现的切片合同，不是今天执行的代码任务。今天只静态核对路径、链接、类型/字段/错误一致性与独立审查，不运行上述Rust/前端构建来伪装插件实现已经存在。旧设计的六次提交保留在新分支历史中，旧分支删除不丢失需求来历；主checkout的未提交审计文件不纳入此分支。
