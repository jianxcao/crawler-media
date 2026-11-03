# ADR-0013 受控插件运行时与工作流扩展

状态：提议，2026-11-03；未实现，未取代当前生产行为。

设计：[完整方案](../plugins/README.md)。基线 `52522549`，重写旧 `6836a4eb` 的提议。

## 决策

以 WASM Component + 精确版本 WIT 承载第三方插件；通过类型化 Hook 和 Provider 扩展 Site、Subscribe、搜索、下载、Transfer、Scrape、通知与界面。第一方 Check-in 和刷流也使用同一宿主能力合同。YAML profile 继续服务声明式 Site；插件不能任意替换宿主模块。

本提议在实施并验收后扩展 ADR-0002 的“第三方加载器以后再说”，以及 CONTEXT 中第一方编译期插件的范围。按用户明确要求，也扩展 ADR-0007 的“搜索只走 HTTP”：插件可申请 `browser:search` 或 `browser:maintenance`，经实例开关、Site 用途 opt-in、插件授权及实际网络隔离能力检查后调用窄 BrowserSession API。默认仍关闭，不向 guest 开放原始 CDP socket、任意脚本或文件访问。真实 Browser egress 隔离不足时拒绝第三方使用，不能仅凭 URL 拦截宣称沙箱成立。

插件只接收脱敏、有界、带 revision 的数据。Filter/Gate 返回白名单 patch/allow/reject/defer；宿主再次检查权限、生命周期与版本。业务写 import 只接受异步 command 并返回 operation_id，独立 worker 执行原有应用服务；禁止在 guest 线程同步重入其他插件。显式拒绝与 runtime failure 分开；默认插件故障保持宿主行为，Admin 的 closed 策略持久 defer 并有次数/时间上限。

沿 ADR-0008 保留五文件边界：app 管插件配置/授权/操作，subscribe 管下载与刷流容量/榜单/候选，library 管文件/Media/Playback与持久文件操作，jobs 管调度执行，cache 只管可重建响应。Store/Queue 各库仍唯一 writer，读复用现有 ReaderPool，禁止实际 writer 嵌套；guest/网络/文件 IO 期间不持数据库锁或 reader lease。跨库写用持久意图和对账，不宣称原子提交。

下载提交记录 Unknown 并对账，不按超时盲目重发。刷流预算是磁盘占用，容量预留与归属同库更新，foreign任务不能认领；delete-task保留文件且不释放磁盘预算。删除文件复用带身份/版本证据的宿主持久操作；ADR-0012 的 Library removal 保留磁盘字节，不能写成回收站。Library删除立即取消相关插件调用，迟到结果不能发布。

运行时采用有界同步池、每调用独立实例、内存和fuel限制、epoch墙钟期限、宿主IO deadline。超时不等于线程已经结束，实际退出前不释放容量。禁用/升级先关闭新准入并drain，处理不确定副作用后才确认完成；精确ABI和state schema不匹配拒绝，没有旧Bus/Downloader适配、双MAJOR或自动数据库/KV迁移。

安装前校验包签名/哈希/manifest/world/权限，默认停用。界面只渲染宿主固定表单与widget，管理API仅Admin；业务endpoint鉴权继承调用主体。凭据由宿主注入，不提供明文读取API。可靠业务事件采用所属库outbox，至少一次投递，插件按event_id去重；高频进度允许明确的best-effort。

## 理由与代价

需求已包含第三方自定义Site、全流程干预、CDP搜索、签到和刷流，纯无载荷Hook无法满足。WASM缩小插件可访问能力，但Browser、Downloader和HTTP必须各自具有真实出站边界，不能从“guest无socket”推导所有间接IO安全。

运行时、WIT、Browser egress、持久命令和UI增加构建与维护成本；先交付小的可验收垂直流程，再开放业务写和网络能力。选定工具链和版本必须用实际生成绑定、打包样例与macOS/Linux构建验证，旧方案样例不作为通过证据。

备选：仅原生扩展无法支持第三方独立分发；内嵌脚本需要另一套沙箱；dlopen缺少隔离；外部服务可作为后续独立可信集成，不作为本版运行时。
