# CDP／Obscura：已落地设计与独立验收门

**2026-10-20 整理，源码基线 `bd6c3c8`。** 原功能分支已由 `14fdbb6` 合入；不再按旧 worktree／分支、预计编译失败和建文件步骤重新执行。当前操作合同见 [ADR-0007](<../../adr/0007-cdp-browser-opt-in.md>)、[部署](<../../deploy.md>)和 `CONTEXT.md` 的 Browser。本文不把所有实现／平台宣布为完整验收，也不是存储优化的前置任务。

## 1. 替代与历史证据

- 原r2–r8多次修订的共享cookie jar、`/json/new`／`/json/close`、旧schema升级或删除旧目录指令已经被取代，不能执行。r9“v7原地升级”也已被当前Store布局入口取代。
- 当前Store只初始化空文件或重开当前application_id/user_version，拒绝旧布局；应用不自动删除数据、不ALTER/backfill旧Site。保留旧目录与一致备份，在独立新目录验证；生产切换另行批准。依据 `crates/store/src/open.rs`、`schema/layout.rs`。
- 每会话独立BrowserContext：createBrowserContext→createTarget{browserContextId}→attachToTarget{flatten:true}→disposeBrowserContext。不支持隔离的端点在导航前报告 incompatible，不能降为共享jar补救。源码 `crates/indexer/src/cdp_page.rs`。
- 原完整r2–r9设计、各任务断言与资产哈希表可由 `git show bd6c3c8:docs/superpowers/plans/2026-10-16-cdp-obscura-managed-browser.md` 追溯；当前资产常量以 `crates/api/src/obscura_install.rs` 为准，不再在实施稿复制易漂移表。
- ADR的 **2026-10-16 Obscura v0.2.3 x86_64-linux T0表**保留原实测：`/json/new`／`close`不支持、BrowserContext隔离／render／受管start-stop局部通过、公共后缀cookie拒收未测。`/json/version`返回Chrome版本不等于Obscura版本，实际核对用 `obscura --version`。本次未重跑，不推广成全部平台／当前全流程通过。

## 2. 保留的功能合同（原T1–T10）

| 原任务 | 当前职责／保留要求 |
|---|---|
| T−1 | ADR／deploy／CONTEXT同步；r8旧描述不是现在的最终合同 |
| T0 | 真实引擎能力探针与ADR实测表；已存在 `crates/indexer/tests/obscura_t0.rs` ignored探针，不重新创建旧 `obscura_compat.rs` |
| T1 | Browser render真实cookie、独立context、全局并发默认3（1..8）、同可注册域串行，FIFO／deadline／close收尾 |
| T2 | 单开关、搜索HTTP-only、Site `maintenance_via` 默认http；按当前新库DDL，不恢复旧schema迁移 |
| T3 | Site／实例端点写入校验：仅http discovery，拒https/ws/wss、凭据／query；显式非法端点不静默换下一跳 |
| T4 | 受管安装与进程身份；固定发布归档SHA-256／大小、临时文件／安全解包、只认自己子进程、随机回环端口、正常停机 |
| T5 | 状态／start／stop路由、保存／Site变更触发BrowserReconcile；请求不等待安装，Job执行时重读desired state |
| T6 | env只seed未设置默认值、请求provider读实时配置；boot enqueue、shutdown停止自己daemon，存活端点不用每请求TCP探活 |
| T7 | hooks共用SiteResponsePolicy；业务成功后合并真实回读cookie，None保留原值，不造 `session=browser` |
| T8 | login／check-in手动与后台统一选择transport；仅导航前失败允许HTTP fallback，不重发Hook；导航后失败原样报告 |
| T9 | sync-cdp使用同一开关：实例端点→本进程受管端点，无端点明确错误，不猜端口 |
| T10 | Web单开关＋实例端点＋真实受管状态／控制／安装策略，Site选择http/browser，代理冲突可见；Node≥22 |

### 端点、cookie与资源

- 关闭开关：不探测／下载／启动Browser；Site自带cdp_url也不生效；登录／Check-in走HTTP。搜索／RSS永远HTTP，Profile `render`忽略并warn，不能通过插件YAML触发浏览器。
- 开启且Site选browser：Site cdp_url→实例cdp.url→已确认本进程存活的受管端点。显式端点连接失败不再换Obscura；非法配置／设置读取错误直接报错，不吞成关闭。可恢复导航前失败warn后HTTP，HTTP失败仍是失败。
- 只经Network.setCookies按Site URL host-only注入，不用ExtraHTTPHeaders；只读当前Site URL cookies并按name合并，同名host-only优先。Obscura T0表记录其path过滤不可靠，当前适配只取path=`/`；不承诺任意第三方引擎行为一致。
- 不枚举／删除第三方cookie、不发delete／clear命令。结束dispose自己context，会话cookie不会留给下一会话；“不删全局jar”不等于“会话cookie永久保留”。SSO跨host状态不保证回写Site。
- normalize_host统一大小写／尾点／IDNA／IP；PSL可注册域分组（localhost/IP/单段名有明确规则），同域串行、无关域可并发。默认总deadline30秒含排队、queue cap15秒、最少剩余10秒、close cap2秒；不合法时间配置报错不静默钳制。
- Browser不使用Site代理；写入后不变式 browser＋非空proxy→400，切回http同时设proxy允许。cookie/token不进日志；结构化日志区分配置意图、降级与进程事实。

### 受管进程与供应链

- 仅switch_on、无实例端点、存在启用且无自带cdp_url的browser Site才启动。browser_sites与managed_browser_sites口径不同，零managed时不启动；状态显示switch_off／instance_endpoint_configured／no_managed_browser_sites／not_installed／start_failed。
- 默认preinstalled不联网，Admin显式download才下载；env只设置未设置过的默认值。下载归档以代码固定SHA-256／size为信任根，200MiB上限，拒额外条目、链接／路径穿越，失败清临时文件；当前preinstalled另支持部署者清单，来源保证不同，按deploy操作，不恢复“只能原归档”的旧限制。
- 随机回环端口，启动／停止互斥，端口竞争有限重试；只有本manager子进程存活且身份确认才返回受管cdp_url，不杀外部服务。pidfile孤儿回收必须核对exe；正常shutdown收尾。host网络下回环CDP仍可被同宿主进程访问，容器／netns隔离另行设计。
- 配置与Queue跨库不假称原子；入队失败error，boot reconcile重试当前desired state。stop同时关开关，不能只杀进程导致后台重启；保存配置不等于进程已就绪。

## 3. 未关闭的验证与独立后续

1. **公共后缀cookie拒收仍未测。** 必须在真实Obscura探针中记录；若引擎接受此类cookie，按ADR报告不兼容，不以全局cookie删除补救，不用局部render通过掩盖。
2. 受支持平台／架构安装、错误哈希／超限／畸形归档、preinstalled零网络、外部端口／pidfile不误认、退出后端点失效与正常shutdown，分别有fake回归及按授权环境的实机证据；T0仅覆盖当时linux资产。
3. 用真实公开入口验证：关闭但Site自带端点不调用opener；搜索render overlay不启Browser；代理冲突双向约束；导航前fallback一次／导航后不重发；真实cookie回读／业务policy／并发隔离；设置立即生效、未知值不吞错、BrowserReconcile重载最新状态。
4. 端到端人工验收在独立临时目录和授权Site执行，确认回环绑定、随机端口、安装策略、不同域并发／同域串行、会话context隔离、cookie持久化、关开关与pidfile／进程退出。不删除实际实例目录，不把旧目录拒绝启动误写成已迁移／已清理。
5. 后续插件方案仍须禁止插件设置Site maintenance_via／cdp_url；CDP discovery／连接应纳入net-guard实例策略，浏览器网络本身不经过net-guard；exec-ctx／统一blocking-budget deadline和受限网络隔离是独立后续，不因本次存储计划而自动实现。
6. 每次升级Obscura重新核对归档信任根并运行真实兼容探针；没有环境写Not run。本次文档整理不运行测试／Node／真实Browser，不修改原历史观察结果。
