# 插件作者与组件 API 契约

> 设计状态：提案，基线 `52522549`（2026-11-03）。本文中的 `crawler:plugin@1.0.0`、组件运行时、SDK、打包器和开发 CLI 均待实现；示例没有在已发布 SDK 上编译验证。
> 配套：[总体设计](../superpowers/specs/2026-11-03-plugin-system-design.md)、[管理界面与 HTTP API](ui-and-api.md)、[实施计划](../superpowers/plans/2026-11-03-plugin-system-plan.md)。本文定义作者契约，不把提案接口写成仓库现有能力。

## 1. 先选扩展方式

| 要实现的能力 | 包内容/组件 world | 宿主接入方式 |
|---|---|---|
| 修改已有站点的选择器、分类映射 | `sites/*.yaml`，无需 WASM | 安装校验后注册有来源的 Indexer profile |
| PT 搜索结果过滤、重评分、重排 | `hook-plugin` | 手动搜索和 Subscribe 共用 Filter 链 |
| 特殊 JSON/HTML 站点、Torznab、自定义 Site | `indexer-plugin` | `search/rss/detail/resolve-download`，输出标准 Torrent |
| Check-in、认证状态校验、会话刷新 | `maintenance-plugin` | Site maintenance Job；第一方 NexusPHP 也实现此契约 |
| 刷流、限额与做种管理 | `task-plugin` + 可选 `hook-plugin` | interval Job 计算策略，提交异步 command，下载仍经完整 Gate |
| 榜单/发现源 | `list-plugin` | 返回 Media 身份线索，宿主确认身份及 Subscribe 策略 |
| 通知渠道 | `channel-plugin` | 接收宿主选择投递的通知，按声明的 HTTP 目的地发送 |
| 插件自己的后台操作入口 | `endpoint-plugin` | 宿主认证的端点，返回受限 JSON 或 operation-id |

一个包可以带多个组件，每个组件选择一个精确world；一个组件不通过“推测导出集合”自动获得其他world。组件各自拥有权限上限。无凭据包可共享global KV；凭据绑定包只能在宿主`security_scope=(site_id,credential_epoch)`内共享KV/blob、配置秘密、读取视图、operation结果和guest输出；A域数据不能被B域/global任务/低权限endpoint取回。人工替换凭据或URL换域，正常同Site会话刷新不换域。凭据包整体出站只允许当前域的Admin授权目的地；不能用拆组件或跨调用保存数据规避。

Indexer 只负责获取与解析 Torrent；Check-in 是独立 Plugin 能力，不作为搜索的隐式副作用。内置 Indexer、Downloader、Library 等仍是宿主模块。简单站点优先声明式 profile；必须执行站点逻辑时再编写 WASM。

## 2. 包结构与精确版本

```text
org.example.site-tools-1.0.0.cmplugin        # 有上限的 ZIP；不直接执行包内文件
├── plugin.toml
├── components/
│   ├── policy.wasm
│   ├── tasks.wasm
│   └── maintenance.wasm
├── schemas/config.schema.json
├── schemas/site.schema.json
├── sites/example.yaml                     # 可选，遵循宿主 Profile schema
├── README.md
├── LICENSE
└── icon.png                               # 可选，宿主重新解码
org.example.site-tools-1.0.0.cmplugin.minisig # 对包原始字节签名，包外携带
```

插件 `version` 是发布版本；`api` 是精确 WIT 包版本。`api = "1.0.0"` 只接受 `crawler:plugin@1.0.0`，不接受 `^1.0`、区间、缺省版本或“尽力兼容”。宿主发行版公布允许的 `(api, world)` 集合，未列入即拒绝安装；不建设双 MAJOR 适配层。新增、删除、修改导出都重新发布精确契约。

包身份是 `id + version + sha256`。同版本不同 hash 拒绝覆盖；不能以相同版本静默替换代码。`generation` 是宿主每次激活生成的执行代次，不由插件填写，也不等于插件版本。配置保存生成独立 `config_revision`。

### 2.1 严格 manifest 示例

下面是“PT 结果规则 + 刷流任务”的完整声明示例；`plugin.toml` 使用闭集 schema，任一级未知字段均报错。

```toml
manifest_version = 1
id = "org.example.traffic-policy"
name = "PT 结果与刷流策略"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Example Author"]
description = "按促销、做种人数和限额选择 Torrent"
config_schema = "schemas/config.schema.json"
state_schema = 1

[[components]]
id = "policy"
entry = "components/policy.wasm"
world = "crawler:plugin/hook-plugin@1.0.0"
permissions = ["hooks:search.after", "hooks:subscribe.score", "kv"]

[[components.hooks]]
id = "search.after"
priority = 400

[[components.hooks]]
id = "subscribe.score"
priority = 400

[[components]]
id = "tasks"
entry = "components/tasks.wasm"
world = "crawler:plugin/task-plugin@1.0.0"
permissions = ["kv", "read:sites", "read:downloads", "read:history", "read:boost",
  "command:indexer.search", "command:downloader.add",
  "command:downloader.control", "command:downloader.delete-task"]

[[components.tasks]]
id = "traffic-cycle"
input_schema = "schemas/config.schema.json"
schedule = { kind = "interval", seconds = 1800 }
concurrency_key = "traffic-cycle"

[limits]
memory_mib = 64
fuel_limit = 10000000
io_ms = 1500
wall_ms = 2000
http_calls = 8
response_bytes = 5242880
kv_bytes = 1048576
```

| 字段 | 校验与含义 |
|---|---|
| `manifest_version` | 必填整数，只接受 `1`；格式变化直接发布新版本，不自动迁移 |
| `id` | 3–128 字符、小写反向域名格式；安装后不可修改，不含路径分隔符 |
| `name/description/authors/license` | 必填展示值，有长度上限；文本转义后渲染，不允许 HTML |
| `version/api` | 必填精确 SemVer；`api` 必须命中宿主允许集 |
| `config_schema/state_schema` | 前者为包内相对路径，后者必填正整数；不得使用远程 `$ref` |
| `components` | 0–16个，`id`包内唯一；entry必须是已声明的普通`.wasm`文件。0个时必须包含至少一个合法`sites` profile，作为YAML-only包；空包拒绝 |
| `world` | 精确全名；组件类型、真实 imports/exports 与 world 一致才接受 |
| `permissions` | 闭集能力名称；声明是申请上限，安装授权和调用 scope 仍需逐次检查 |
| `hooks` | 仅 hook world 可声明；ID 必须存在，priority 为 0–1000；重复 ID 拒绝 |
| `tasks` | 仅 task world；ID 唯一、输入 schema 有效；interval 为 60–86400 秒 |
| `indexers/maintenance/lists/channels/endpoints` | 对应 world 的注册描述；跨 world 声明拒绝；每类最多 32 个 |
| `http` | 可选目的地数组：`{origin, path_prefix, methods}`；origin 不含凭据，不接受任意通配域 |
| `credential_binding` | 凭据组件必填 `"invocation-site"`；其所在包的所有出站都限定当前 Site 授权目的地 |
| `sites` | 可选 profile 路径列表；使用插件命名空间，任何 ID 冲突直接拒绝 |
| `limits` | 正整数申请值；执行取宿主默认/封顶与申请的较小值，不得借配置扩大；fuel 是指令预算，示例数值需实测校准，epoch/wall 是墙钟截止而非 CPU 计时 |

`indexers`描述含`{id,name,kind,capabilities,site_schema}`；capabilities是`search/rss/detail/resolve-download/user-stats/categories`的闭集。`maintenance`含`{id,actions}`，actions为`verify-session/check-in`；Site能力目录合并两类已注册能力。`endpoints`含`{id,method,path,auth,input_schema,capabilities}`，capabilities是组件已声明read/command能力的子集，也是API key可获委托的上限；auth只允许`admin/user/apikey`，apikey只接收header token，不接受query token，禁止public。声明中没有Cookie、CDP endpoint、Downloader密码、任意shell、cron或任意JavaScript。

### 2.2 配置 schema

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object", "additionalProperties": false,
  "required": ["sites", "downloader", "max_active", "budget_bytes"],
  "properties": {
    "sites": {"type": "array", "minItems": 1, "maxItems": 16,
      "uniqueItems": true, "items": {"type": "string"}, "x-ui": {"widget": "site"}},
    "downloader": {"type": "string", "x-ui": {"widget": "downloader"}},
    "max_active": {"type": "integer", "minimum": 1, "maximum": 20},
    "budget_bytes": {"type": "integer", "minimum": 0},
    "dry_run": {"type": "boolean", "default": true}
  }
}
```

宿主支持有界 object/array、string、boolean、integer、number、enum、required、长度/数值范围和本地 `$defs/$ref`；拒绝远程引用和递归 schema。`x-ui` 是固定组件提示，不影响权限。后端和前端使用同一校验规则，默认值只在首次创建时显式物化；更新时缺失字段报错，不从旧版本配置补值。Site ID/Downloader ID 选择器只展示已授权范围，保存时再次检查。

秘密值保存在宿主凭据槽位；插件配置仅持有槽位引用，`config.get` 返回非秘密配置。管理 API 写秘密使用 `keep/replace/clear` 显式动作，读取仅返回 `configured`。`format: password` 不是安全能力，也不意味着插件可以读到密码。配置 schema 不匹配的新包不能直接激活。

## 3. 权限、主体和调用上下文

### 3.1 有效授权

有效能力 = 包声明 ∩ 组件声明 ∩ Admin 实际授权 ∩ 当前主体权限 ∩ 当前调用上下文 ∩ 尚未撤销的 scope。插件任务使用专属系统主体，不能因为由 Admin 安装便读取所有 User 的数据；endpoint 派生的命令继续使用已认证请求主体。插件不能自报 `user_id` 改变主体。

apikey endpoint使用宿主固定的EndpointKey主体（key_id+issuer_user_id），委托记录包含endpoint_ids、capabilities、object_scope和expiry。有效权限还必须与当前issuer权限交集，不能把key调用改成全局system；读取operation只限原key主体，轮换不继承。主体/委托由管理API签发，guest不接收secret或自造issuer。

| 权限族 | scope 与边界 |
|---|---|
| `hooks:<id>` / `events:<domain>` | 指定 Hook 或 Notify 域；没有模糊 `*` 自动扩权 |
| `read:sites/subscribes/library/downloads/history/catalog` | 独立实体 ID 集与字段投影；用户维度字段另需该用户同意 |
| `kv` | 仅当前plugin_id+security_scope空间、固定配额；只允许同域组件共享，插件不得选择或枚举其他域 |
| `blob` | 同一security_scope的有界BLOB，单对象1MiB、插件总50MiB；不提供文件路径 |
| `read:boost` | 当前插件获准Site/Downloader范围的容量、归属、reservation与磁盘证据 |
| `notify:send` | 固定事件投递或获准task/endpoint通知；凭据域任意正文不得流向域外 |
| `http` | manifest 精确 origin/path/method ∩ Admin 授权；私网目的地仅 Site 实例授权 |
| `http:site-auth` | 当前 Site 的宿主凭据注入；不能指定任意 Site ID，不能读取注入值 |
| `browser:search` / `browser:maintenance` | 全局 Browser 开关、Site 对该用途的 opt-in、插件 grant 三项同时成立 |
| `indexer:provide` / `maintenance:provide` | 只提供声明的适配器；不授予其他 Site 的凭据或写权限 |
| `indexer:yaml` | 注册包内命名空间 profile；不开放覆盖内置/其他插件 ID 的权限 |
| `command:<kind>` | 指定 command 的实体 scope；创建默认绑定当前插件来源，不能伪造所有权 |
| `command:downloader.delete-files` | 独立敏感授权；只接受宿主持久文件操作计划，不能传任意路径 |
| `list:provide/notify:channel/endpoints` | 仅声明的列表/渠道/端点；通知渠道不会获得业务写权限 |

授予 `command:downloader.delete-task` 不授予删除其他插件/手动添加任务的权力；本版不提供 `foreign` 批量管理能力。任何跨 Site、跨 User、跨 Library 行为先检查 scope，再访问数据。授权撤销立即阻止下一次 import 与 command 执行；取消在途调用，不等待旧 generation 自然退休。

### 3.2 InvocationContext

```text
InvocationContext {
  invocation_id, cause_id, plugin_id, generation, config_revision,
  subject, site_id?, subscribe_id?, library_id?, deadline, cancellation
}
```

此结构仅由宿主创建，作为只读视图和内部 capability 绑定传入。`subject` 是已验证的主体引用；`cancellation` 是宿主 token；`deadline` 是不可延长的绝对期限。插件返回同名字段、把 context JSON 放进 command，或复制别次 invocation 的 handle，均不能覆盖真实上下文。

每次调用创建独立组件实例，WASM 内存不作为跨调用状态。宿主在专用有界同步池中执行插件；网络等待计入该调用预算，队列满立即返回 `busy`。插件不得假设每个导出在同一线程或实例运行。同步 Filter/Gate 不持有数据库 reader lease/writer 锁进入 WASM，不允许锁跨网络、文件 IO 或 await。

`site.request.before/site.response.after`是受限transport子上下文：宿主暂停父guest并在当前线程内联独立子Store，深度1，不入池、不等待全局permit；父子内存仍计额，同插件借父permit，其他插件permit与子内存仅try-acquire。该子上下文只允许runtime/clock/config/当前脱敏输入/log，本文标“全部”的其他imports在这里均wrong-context，尤其禁止HTTP/Browser/KV/数据库/command/notify。Browser自动子资源只执行静态net-guard，不运行这些guest Hook；普通插件同步重入仍禁止。

记号：`F`=Filter，`G`=Gate，`N`=Notify，`I`=Indexer，`M`=maintenance，`T`=task，`E`=endpoint，`L`=list，`C`=通知渠道，`U`=状态视图。下文的 context 列是允许调用的上下文，仍需单独授予权限。

### 3.3 错误与副作用通则

`ApiError {code,message,detail?,retry_after_ms?,operation_id?}` 的 code 闭集为 `permission-denied/wrong-context/not-found/invalid-argument/stale-handle/stale-generation/conflict/quota-exceeded/busy/deadline-exceeded/cancelled/unsupported/upstream/unknown-outcome`。message 是可展示摘要，不含秘密；错误不会授权重试。`upstream` 附 `auth-expired/rate-limited/challenge/network/parse` 分类。下表中的网络/Browser 专属错误是 detail 闭集，例如 blocked-address 对应 permission-denied、browser-isolation-unavailable 对应 unsupported，不另造不确定的顶层异常类型。

非法返回、trap、超时、越权属于插件失败；网络 429、Site challenge 不等于插件代码错误。Filter 默认保持上一版输入；Gate 的错误策略由宿主配置，严格 Gate 失败可 `defer`，不得把它悄悄当 allow。显式 reject/defer 是业务决定。Notify 失败不回滚已经提交的业务事实。

HTTP、Browser、KV、通知渠道有实际副作用，必须在能力表中标明；“只读业务数据”不等于整个插件无副作用。Notify 不可提交业务 command；它可读数据、记录自身去重状态及投递外部通知，但不能借事件回调创建 Subscribe 或下载任务。

## 4. 宿主 imports 完整目录

### 4.1 基础、读取与网络

所有分页返回 `Page<T>{items,next_cursor?}`，limit 为 1–200，cursor 由宿主签发，不能作为 SQL。读取是各库独立快照；多库结果不承诺原子一致，视图附 `revision/observed_at`，command 必须重新验证。

| import | 请求 → 返回 | 额外错误 | 副作用 / context |
|---|---|---|---|
| `runtime.current-context` | `()` → InvocationContext 只读投影 | — | 无 / 全部 |
| `runtime.cancelled` | `()` → bool | — | 无 / 全部；其他 import 仍自行检查取消 |
| `clock.now` | `()` → UTC 毫秒 | — | 无 / 全部；测试可注入时钟 |
| `log.write` | `{level,message,fields}` → unit | quota-exceeded | 有界审计日志 / 全部；禁止请求体/凭据输出 |
| `config.get` | `()` → `{revision,validated_json}` | stale-generation | 无 / 全部；本次调用固定 revision |
| `kv.get/list` | `key` / `{prefix,cursor,limit}` → value+etag / Page | not-found | 当前插件空间只读 / 全部 |
| `kv.put/delete` | `{key,value?,if_match?}` → new_etag / unit | conflict,quota-exceeded | 自身 KV 原子写 / 全部；F/G 推荐不使用 |
| `blob.get/list` | key / `{prefix,cursor,limit}` → bytes+etag / Page | not-found,body-too-large | 自身命名空间；单对象≤1MiB / 全部 |
| `blob.put/delete` | `{key,bytes?,if_match?}` → etag / unit | conflict,quota-exceeded | 自身BLOB与quota同库事务，插件总额≤50MiB / T/E |
| `notify.emit-event` | `{event_id,channel_ids}` → delivery_operation_id | permission-denied,quota-exceeded | 只投递宿主已有事件的固定模板 / N；不接受任意正文或递归渠道 |
| `site.list/get/stats` | scope 过滤 / site_handle → SiteView/StatsView | not-found | 无 / 全部；无 Cookie、proxy、CDP URL |
| `subscribe.list/get` | filters / subscribe_handle → SubscribeView | not-found | 无 / 全部；按主体过滤 |
| `library.list/media/files` | library_handle、media_handle、分页 → LedgerView | not-found | 无 / 全部；默认无绝对路径，无隐式 probe |
| `download.list/get` | downloader_handle、状态过滤 / task_handle → TaskView | not-found | 无 / 全部；含证明所有权、HR、observed_at |
| `boost.snapshot` | scoped Site/Downloader集 → `{revision,used_bytes,reserved_bytes,retained_file_bytes,reservations,tasks,disk:{available_bytes?,observed_at,status}}` | stale-handle,permission-denied | 宿主持久事实与有时效磁盘证据，未知非0；分页按通用Page / T/E |
| `history.query` | kind、实体 handle、时间范围、分页 → HistoryView | invalid-argument | 无 / 全部；不能查询其他用户观看历史 |
| `catalog.lookup` | 已知 Media/外部 ID → 缓存 CatalogView | not-found | 无 / 全部；缓存 miss 不隐式发网 |
| `indexer.cached` | 已完成 search_operation_id、分页 → TorrentView | stale-handle | 无 / T/E；不内联再调用 Provider |
| `operations.get/list` | operation_id / scope 过滤 → OperationView | not-found | 无 / 全部；看不到别的主体操作 |
| `http.fetch` | HttpRequest → HttpResponse | blocked-address,body-too-large | 外部 HTTP / 全部；F/G 仅 GET，仍需相应授权 |
| `http.fetch-many` | 1–8 个 HttpRequest → 按输入顺序的 result 列表 | 同 fetch | 有界并行，全部共享剩余预算 / 全部 |

`SiteView` 含 `{handle,id,name,enabled,profile_ref,capabilities,maintenance_policy}`；`TorrentView` 含 `{handle,site_id,title,size,seeders?,leechers?,published_at?,free?,hr?,release?,score?,labels}`。未知促销/HR/seeders 使用 option，不把未知当成免费、无 HR 或零做种。下载 URL、passkey 和凭据不出现在 view。

`HttpRequest {method,target,headers,body?,max_bytes}` 的 target 是 `public-url` 或 `site-relative{path,query,auth_slot?}`；Site ID 来自真实上下文。宿主禁止覆盖 Host/Cookie/Authorization/代理头；auth_slot 只能引用已绑定槽位。F/G 仅允许 GET，不允许借 HTTP 执行站点签到等业务动作；需要此类副作用的实现放在 maintenance/task。`HttpResponse {status,headers,body,truncated=false,receipt?}`；超出上限报 `body-too-large`，不把半截 JSON 当成功。

宿主检查 URL、DNS 结果和实际连接地址，每跳重定向复核授权；HTTPS 不降级。私网授权仅对对应 Site 的精确 host/port/path 生效，云元数据地址不可授权。插件返回的下载/图片 URL 也走同样检查，禁止把未检查 URL 交给 Downloader 或浏览器二次获取。

带凭据的响应可能包含未知编码、站点新生成token或页面内秘密；脱敏只能降低泄漏风险。所有原始输入、状态、读取与输出保留宿主security_scope，包的全部出站受当前域限制；日志/UI仅限该域Admin，外部通知只用宿主固定模板，endpoint不能向低权限主体返回认证内容。只有经宿主规范化的标准Torrent/Media公开字段允许释放到共享业务结果，原始HTML、任意扩展JSON和资源句柄不能释放。恶意Provider仍可能把秘密编码进公开字段，授权它处理Site认证内容包含此信任；这些限制不构成隐写检测或通用信息流证明。

### 4.2 Browser/CDP 导入

当前仓库 Browser 主要提供 Site 维护的 render 通道，ADR-0007 规定搜索/RSS 走 HTTP。以下是按本次需求提出的扩展契约：插件可以用受控 Browser 搜索和签到；实现需同步记录新的 ADR 决议，不能宣称当前代码已经支持。RSS 默认 HTTP；确需浏览器读取时同样按 `browser:search` 授权，不能暗中回退。

| import | 请求 → 返回 | 错误/约束 | 副作用 / context |
|---|---|---|---|
| `browser.open` | `{purpose}`（search 或 maintenance）→ Session resource | disabled,busy,browser-isolation-unavailable | 创建当前 Site 独立 BrowserContext / I 或 M |
| `session.navigate` | `{site_relative_url,wait_until}` → NavigationReceipt | blocked-address,navigation-unknown | 发出页面请求；已有导航不再自动 HTTP 重试 |
| `session.dom` | `{max_bytes}` → 受限 DOM snapshot | body-too-large | 读取页面；脚本/隐藏敏感节点按宿主规则裁剪 |
| `session.query` | `{selector,limit}` → list<ElementHandle,visible_text> | invalid-selector | 只读；handle 限当前文档，不返回任意属性全集 |
| `session.text` | `{element,max_bytes}` → visible_text | stale-handle,body-too-large | 只读；不读取隐藏脚本或凭据输入 |
| `session.attribute` | `{element,name}` → option<string> | stale-handle,permission-denied | 仅 href/value 等字段白名单；秘密输入值不可读 |
| `session.click` | `{element}` → ActionReceipt | stale-handle,action-unknown | 可引起导航/表单提交；网络仍受同一策略 |
| `session.fill` | `{element,value}`（text 或 credential_slot）→ unit | permission-denied | 填表；槽位由宿主填入，插件不能取回秘密 |
| `session.wait-for` | `{selector?,condition,timeout_ms}` → WaitOutcome | deadline-exceeded | 有界等待；不得超过调用剩余期限 |
| `session.read-response` | `{navigation_receipt,max_bytes}` → HttpResponse | stale-handle,body-too-large | 仅当前会话已授权响应的脱敏副本；无任意网络监听 |
| `session.capture-session` | `()` → opaque SessionCapture | permission-denied | 仅当前 Site cookie 回读到宿主暂存 / M |
| `session.close` | `()` → unit | cleanup-failed | 关闭 page 并 dispose 当前 context；可重复 |

不提供 `evaluate JavaScript`、raw CDP command、浏览器文件读写、任意 screenshot/下载、网络监听或 cookie 明文读取。DOM/元素句柄不能跨页面和调用复用。会话结束、trap、取消、generation 退休都触发宿主清理；销毁失败记审计并隔离该会话，不复用不干净 context。

授权不是只检查顶层 navigate：浏览器网络隔离必须覆盖重定向、iframe、脚本、图片、fetch、WebSocket、worker、预取和其他自动连接，并禁持久 service worker/WebRTC 等直连旁路。所有 TCP/DNS 出站经宿主验证目的 IP 的 egress，并阻断绕过代理的直连；CDP URL interception 本身不足。真实 transport/代理隔离未完成或无法证明生效时 `browser.open` 返回 `browser-isolation-unavailable`；外部 CDP 同样受此门槛。

复用 ADR-0007 的全局限额、同可注册域串行、host-only cookie 注入、会话独立 context、定向 cookie 回读、取消与总期限。只允许导航前失败按宿主已选策略回退 HTTP；导航发出后未知结果返回 `unknown-outcome`，不双重签到。`SessionCapture` 只在导出结果中交给宿主校验，随后宿主在该 maintenance operation 中另行执行 Gate、Site revision CAS 和结果持久化；其完成之前不发完成 Notify。插件不直接更新 Site 表；Cookie CAS 冲突不抹掉站点已签到事实、不重做签到。

### 4.3 Command：所有业务写入统一异步

`commands.submit(CommandRequest{kind,payload,idempotency_key,expected_revision?}) → Accepted{operation_id}` 仅在 `T/E` 可用。维护导出的 session proposal 由宿主在调用结束后作为既有 maintenance operation 的持久阶段执行，不开放任意 cookie 写入口。任务需要立即搜索时也提交 `indexer.search`，不能在 WASM import 内同步重入其他 Provider。

| command kind | payload 核心字段 → 成功结果 | 权限/实际副作用 |
|---|---|---|
| `indexer.search` / `indexer.rss` | sites、query、page → candidate_batch_handle | 对 Site 发请求、运行完整搜索 Hook；不自动下载 |
| `site.maintenance` | site_handle、action → maintenance_outcome | 独立 worker 调 maintenance-plugin，允许更新验证过的会话 |
| `site.create/update/delete` | 非秘密Site草案/handle+revision → Site/删除receipt | 需独立command授权，沿Site前置Hook；不能创建grants/凭据，未知新目的地保持停用待Admin批准 |
| `downloader.add` | candidate_handle、downloader_handle、options → submission_outcome | 同一 DownloadService，执行全部 Gate 与额度检查 |
| `downloader.control` | owned_task_handle、pause/resume/tags/category/limits、revision → TaskView | 仅可证明归属的任务；不开放全下载器全局限速 |
| `downloader.delete-task` | owned_task_handle、revision、reason → receipt | 必须保留文件；复查 HR/保护期与归属 |
| `downloader.prepare-cleanup` | owned_task_handles、expected_revision、reason → host_file_plan_handle | 锁外观察磁盘/引用与精确归属，再持久化计划；不删文件，需独立command授权 |
| `downloader.delete-files` | host_file_plan_handle、revision、reason → FileOperationReceipt | 独立敏感 scope；宿主真实持久文件操作，不是 delete-files 布尔开关 |
| `subscribe.create` | Media handle、范围、Filter、Site、Library 绑定 → Subscribe handle | 宿主设置来源/owner，创建前 Gate 完整执行 |
| `subscribe.update/delete/complete` | owned_subscribe_handle、revision、白名单 patch/reason → receipt | 复查主体、引用、完成事实；不能修改 owner/source |
| `library.refresh` | library_handle、bounded selection → Job handle | 经现有准入/取消逻辑；无任意 FS 路径 |
| `transfer.request` / `scrape.request` | 宿主签发文件集合 handle、Library、options → Job handle | 仍经命名/Transfer/Gate、库 generation 与文件操作护栏 |
| `notify.send` | title、body、level、link?、channel_ids → delivery receipt | 外部通知，宿主转义、限速；不接受任意用户 token |
| `schedule.set/disable` | declared_task_id、interval seconds/enabled、revision → definition receipt | 仅自身 manifest 声明任务；不创建任意 JobKind |

上述全部可能返回通用错误；成功 admission 只说明已记录 operation，不说明业务完成。宿主单独 worker 执行，运行前再检查 grant、主体、generation、实体 revision 和删除/取消状态。候选失效返回 `stale-handle`，插件重新搜索，不能拿旧 handle 重新绑定到新 Site。

cleanup计划持久绑定plugin/subject/security_scope、operation、Downloader任务归属、完整文件身份/版本/引用证据，默认10分钟有效；prepare返回accepted，完成后从operations.get取handle。delete-files必须重新检查全部证据与当前grants/HR/Library状态，过期或改变返回stale-handle/conflict并重新prepare，不能延长旧计划。磁盘余量只来自宿主可信adapter/挂载观测；未知时停止自动准入。

Operation 状态统一为 `queued/running/waiting/blocked/succeeded/rejected/deferred/failed/cancelled/unknown`。终态仅 `succeeded/rejected/failed/cancelled`；deferred 带 next_run_at，waiting 表示已受理阶段等待，blocked 等待条件或人工处理。`unknown` 表示请求可能已到外部系统而确认丢失，属于非终态；`download` 的结果必须保留 `Unknown(operation_id)`，先按本次提交的 info-hash/归属证据对账，不能换 idempotency_key 再 add。需要人工处理时返回明确 attention reason。

`idempotency_key` 在 `(plugin_id, subject, key)` 下唯一，宿主记录 command kind 与规范 payload 的联合 hash；同 key 同请求返回同一 operation，换 kind 或 payload 返回 conflict。attempt、重启恢复与客户端超时都不产生新的业务身份；保留期限不能短于 command 的最大存活/对账期限。外部服务不支持幂等时不声称 exactly-once。

`cause_id` 和 `visited(plugin_id,command_kind,target_id)` 保存在宿主链路中，插件不能提交或重置。command worker 再触发同样 Hooks；创建 command 的插件也不能跳过其他插件 Gate。重复访问项或派生深度大于 4 的提交拒绝并审计；Notify 不能发业务 command，固定事件通知走 `notify.emit-event`，F/G 也没有 command import，从入口上阻断常见循环。

## 5. 插件 exports 与中间步骤

### 5.1 各 world 的固定导出

| export | 请求 → 返回 | 业务错误与副作用 |
|---|---|---|
| `lifecycle.validate` | config/state schema IDs → validation | 只校验；不迁移、不联网、不写业务数据 |
| `lifecycle.health` | 当前 context → Healthy/Degraded{reason} | 只读状态，不能通过此接口清除熔断 |
| `hooks.filter` | 版本化 FilterInput → list<Patch> | invalid-input/unsupported；修改仅经宿主校验应用 |
| `hooks.gate` | 版本化 GateInput → allow/abstain/reject/defer | reject/defer 是正常业务结果；混合 F/G 的 patch 经相应类型化返回 |
| `hooks.notify` | Event{id,kind,occurred_at,payload} → unit | 至少一次可能重复，按 event.id 去重；无业务 command |
| `indexer.describe` | `()` → manifest 中声明的 descriptor 列表 | 运行时不能新增 ID/权限；无 IO |
| `indexer.search` | Site context、query、page/cursor → TorrentBatch | auth-expired/rate-limited/challenge/network/parse |
| `indexer.rss` | Site context、cursor? → TorrentBatch | 同 search；默认 HTTP |
| `indexer.detail` | provider 自有 torrent_ref → TorrentDetail | not-found/unsupported；仍为 Site scope |
| `indexer.resolve-download` | opaque download_ref → DownloadRequest | 返回受限目标/凭据槽位引用；不返回 Cookie 明文 |
| `indexer.user-stats/categories` | Site context → StatsView/CategoryView | unknown 字段保留 option，别用 0 伪造事实 |
| `maintenance.run` | action、Site context、job id → MaintenanceOutcome | completed/already-completed/auth-expired/challenge/unknown；可附 SessionCapture |
| `tasks.run` | task_id、validated_input、job id/attempt → TaskOutcome | 返回进度、operation_ids、下轮摘要；不能阻塞等待自己的 command |
| `endpoints.handle` | 宿主认证后的 method/path/query/body → EndpointResponse | body ≤1 MiB；不透传 Cookie/Authorization，不返回 Set-Cookie |
| `lists.fetch` | list_id、cursor、config → MediaReferenceBatch | 只给别名/标题年份线索与分页；不直接创建 Subscribe |
| `channels.deliver` | delivery_id、Message → DeliveryOutcome | 外部 HTTP；不发 business command，不递归发通知 |
| `ui.status` | `()` → list<StatusItem> | 只读 stat/table/list/progress/chart；无 HTML/JS/iframe |

每个 world 的必需导出在该精确版本 WIT 中固定；不以“缺导出默认为 unsupported”的适配层补齐。声明不支持的 Indexer capability 不会被宿主调用，对应方法若被错误调用仍返回 unsupported，不能 panic。任务可返回 `partial` 并列出已受理 operation；不能把 accepted 汇总成“下载成功”。

### 5.2 Subscribe / 搜索 / Site / 文件的干预输入

完整 Hook ID、时机与类别以[总体设计 §5.2](../superpowers/specs/2026-11-03-plugin-system-design.md#52-全流程干预目录)为唯一目录，下表补充作者如何构造输出，不另外定义别名。Hook 输入均为 `{schema_version:1, context, payload}`；宿主固定 payload schema，不能给未知 Hook 发送任意 JSON。F/G 的改动都是白名单 Patch，N 只能观察提交后的事实。

| 阶段与 Hook | 作者输入 → 输出示例与校验 |
|---|---|
| Site CRUD：`site.create.before/update.before/delete.before` | 非秘密草案/引用概要 → 配置建议或拒绝；不得授予权限、改变凭据 |
| 维护：`site.auth.before/checkin.before/session.before-save` | Site、用途、日期窗口、session revision → allow/reject/defer；永远不含 Cookie |
| 查询：`search.before/search.site.before` | `{keyword,sites,categories,page,purpose}` → query patch、授权 Site 子集；不能自选代理 |
| 单站与汇总：`search.site.after/search.after` | `{candidates,site_failures}` → drop/重排/仅补充自身 Provider 的候选；补充项重走全部准入 |
| 响应：`site.request.before/response.after` | 脱敏请求/有界正文 → 非秘密 query/header patch 或解析建议；宿主再次校验网络/认证事实 |
| 准入：`filter.before/after` | Release建议/已准入集合 → 前置建议重解析，后置drop/有界rescore/set-order；手动/Subscribe/刷流共用，不复活拒绝项 |
| Subscribe 政策：`subscribe.create.before/update.before` | 草案/带 revision patch → 允许字段建议；重新验证 owner、Media、Library 和范围 |
| Subscribe 评分：`subscribe.score` | `{candidates,rescore_cap}` → `rescore{handle,delta}`/drop；已有准入事实不变 |
| Subscribe 选择：`subscribe.select` | `{admitted_handles,chosen,coverage}` → 合法重选子集/reject/defer，不能注入外部 handle |
| 集数与完成：`subscribe.episodes-refresh/completion-check` | catalog 范围/宿主 coverage 与 pending → 范围建议/延迟完成；不能清除已知逐槽事实 |
| 下载：`download.before/control.before/delete.before` | 冻结身份的计划/任务 revision/保护证据 → 白名单 patch 或收紧，不改变归属 |
| Transfer：`transfer.plan/intercept/rename`、`upgrade.decide` | 文件集合、相对路径、旧新质量 → 合法路径建议/收紧替换；全部在 reserve 前 |
| 识别与 Scrape：`media.name-recognize/recognize/id-convert`、`scrape.before` | 名称/别名/sidecar 计划 → 建议；冲突不能覆盖已采用身份 |
| 完成事实：`download.after`、`site.checkin.after`、`metadata.scraped` 等 N | 持久结果与 operation → unit；unknown 不等于 failed，accepted 不等于 completed |

顺序为宿主固定的阶段顺序，阶段内按 `(Admin 配置 priority, plugin_id, component_id)` 稳定执行。后一插件看到前一插件的有效 Patch；每次 Patch 都重新验证。后来的 allow 不覆盖此前 reject、宿主 ACL、HR、路径或 generation 检查。Notify 在对应数据库提交后发布；跨库流程各阶段各发事实，不声称跨库事务或所有事件必达。

候选handle是宿主签发的opaque capability，绑定cause、Site、provider generation、内容摘要和有效期。`set-order`必须是当前存活handle的完整无重复排列；drop单调删除；rescore用宿主下发的累计cap，按plugin/候选跨filter.after与subscribe.score累计，不能注册两个Hook重复加分。最终policy_score与最后有效set-order同时用于手动默认结果排序和后续choose，未指定order时按score降序和稳定候选键；用户页面重排只改展示。

`DownloadPlanPatch` 仅允许授权 Downloader 子集中的目标、已允许根目录内相对子目录、category、插件 namespace tags、paused、每任务速率上限、sequential/first-last-piece。不能改 candidate/source/owner/ACL、info-hash、Site、媒体证据、generation、cause_id 或删除保护。`defer{until,reason}` 必须有有界期限；持久化等待状态并展示，不能在同步池 sleep。

## 6. 最小 WIT 核心与 Rust 工程

以下完整最小包表达 `subscribe.score` 的核心类型和导入/导出关系，用于实现阶段先验证绑定生成。它是文档摘录：完整 API 还需按前述目录展开，不声称此摘录就是可安装的正式 world 或已经编译。

```wit
package crawler:plugin@1.0.0;

interface types {
  enum error-code { permission-denied, invalid-argument, stale-handle,
    deadline-exceeded, cancelled, quota-exceeded, conflict, upstream }
  record api-error { code: error-code, message: string }
  record context {
    invocation-id: string, cause-id: string, plugin-id: string,
    generation: u64, config-revision: u64, subject: string,
    site-id: option<string>, subscribe-id: option<string>,
    library-id: option<string>, deadline-ms: u64, cancelled: bool,
  }
  record torrent-view {
    handle: string, title: string, size: u64,
    seeders: option<u32>, score: s32, labels: list<string>,
  }
  record subscribe-score-input {
    schema-version: u32, context: context,
    candidates: list<torrent-view>, rescore-cap: u32,
  }
  variant result-op {
    drop(string), rescore(tuple<string, s32>),
  }
}

interface runtime {
  use types.{context};
  current-context: func() -> context;
  cancelled: func() -> bool;
}
interface config {
  get: func() -> string;
}
interface log {
  enum level { debug, info, warn, error }
  write: func(level: level, message: string);
}
interface kv {
  use types.{api-error};
  record entry { value: list<u8>, etag: string }
  get: func(key: string) -> result<option<entry>, api-error>;
  put: func(key: string, value: list<u8>, if-match: option<string>)
    -> result<string, api-error>;
}
interface hooks {
  use types.{subscribe-score-input, result-op, api-error};
  subscribe-score: func(input: subscribe-score-input)
    -> result<list<result-op>, api-error>;
}
world minimal-hook-example {
  import runtime;
  import config;
  import log;
  import kv;
  export hooks;
}
```

正式 world 只链接规定的自定义 imports 与明确列入允许集的最小 WASI 能力；拒绝 filesystem/sockets/raw HTTP/process/environment 旁路。固定 import 有未授权能力时宿主提供返回 `permission-denied` 的桩，不能因此授予该能力。组件声明之外的 import 安装时拒绝；允许集不是整个 WASI 默认 linker。

### 6.1 Rust 作者项目

```text
traffic-policy/
├── Cargo.toml                  # edition 2024, crate-type = ["cdylib"]
├── src/lib.rs                  # 导出入口，纯策略函数独立模块
├── wit/                       # 未来发行版提供的精确 WIT 包
├── plugin.toml
├── schemas/config.schema.json
└── tests/fixtures/             # 脱敏 Site 页面、JSON、Torrent 批次
```

下列 Rust 对应上面的最小 WIT，展示生成 bindings 后的入口形态；`wit-bindgen` 版本、组件构建目标、组件化步骤须由未来 SDK 模板锁定并在 CI 验证，不能复制旧方案中的库版本当作当前事实。拟议 `crawler-plugin-sdk` 只包装精确 bindings、错误和测试 harness，不隐藏网络/权限。

```rust
wit_bindgen::generate!({ path: "wit", world: "minimal-hook-example" });
use crawler::plugin::types::{ApiError, ResultOp, SubscribeScoreInput};

struct Policy;

impl exports::crawler::plugin::hooks::Guest for Policy {
    fn subscribe_score(input: SubscribeScoreInput) -> Result<Vec<ResultOp>, ApiError> {
        let bonus = input.rescore_cap.min(5) as i32;
        let ops = input.candidates.into_iter().filter_map(|torrent| {
            match torrent.seeders {
                Some(0) => Some(ResultOp::Drop(torrent.handle)),
                Some(n) if n >= 20 => Some(ResultOp::Rescore((torrent.handle, bonus))),
                _ => None, // 未知做种人数不是 0；保留宿主决定
            }
        }).collect();
        Ok(ops)
    }
}

export!(Policy);
```

SDK 未来应提供 `Context` 只读访问器、typed Patch 构造器、受控 HTTP/Browser wrapper、`CommandAccepted` 和 `OperationState`，不要提供 `unsafe_raw_host`。作者仍须处理 unknown、取消与被撤销授权；SDK 不替作者重试有副作用请求。

## 7. 三个可实施示例

### 7.1 Check-in 插件

第一方 `builtin.nexusphp-maintenance` 封装当前 `crates/hooks/src/plugins.rs`、`site_policy.rs` 的认证验证与签到判定，按同一 maintenance 契约注册；本阶段不把它复制成 Indexer 特例。现有 login 验证已配置 Cookie，不支持凭空提交用户名/密码，文档与 UI 不能称其为通用自动登录。

```text
run(action, site_context):
  读取本轮 Site maintenance_policy、时区及宿主记录的上次确认结果
  若 action=check-in 且该 Site 当日已有确认成功事实，返回 already-completed
  若用 Browser，open(maintenance) 并在同一会话执行导航、必要表单和等待
  否则 http.fetch(site-relative, host credential slot)
  在主响应的可见结果区域验证：challenge / 认证失败 / 明确失败优先
  有明确“今日已签到”证据 → already-completed；明确成功证据 → completed
  无成功证据 → parse/unknown-outcome，不能仅凭 HTTP 200 判成功
  返回 session_capture（若有）及证据摘要；宿主执行持久化和完成审计
```

任务默认 interval 86400 秒；“每天一次”由 Site 时区日历键和宿主成功事实控制，不声称 interval 等价 cron。重复 Job、崩溃恢复、站点返回“已经签到”均应成功去重；错误页面中的帮助文本、隐藏节点、脚本示例不能充当成功证据。提交后超时必须先查询结果页确认，不能立即再次 POST。

宿主业务唯一键为`(site_id,action,window_key)`，plugin/generation/凭据代只记来源。并装、升级、更换Provider或凭据都不能清除成功或unknown；发动作前原子认领同一receipt，unknown未解决时另一Provider也不能重发。时区改变从下一已确认窗口边界生效。

其他 Site 的作者提供自己的响应判定和 HTTP/Browser 流程。需要 Browser 时声明 `credential_binding="invocation-site"`、`maintenance:provide`、`http:site-auth`、`browser:maintenance`，安装授权与 Site opt-in 都通过才启用；第三方也不能读取原始 Cookie 或调用任意 CDP。

### 7.2 刷流插件

```text
traffic-cycle(config, job):
  通过boost.snapshot读取权威已预留/已占用/残留文件字节、磁盘证据和归属快照
  先对账 unknown add/delete；存在未解决 unknown 时保留占用额度
  校验 free 截止时间、HR、最小做种、活动数、磁盘预算/余量和全局限制
  无新鲜候选则 submit indexer.search/rss，记录 operation_id，本轮结束
  有候选则按免费时长、seeders、size 等打分，选择预算内的候选
  dry_run 时输出原因和计划；否则逐项 submit downloader.add，保存返回 operation_id
  下轮查询提交结果，不把 accepted 当成功，不把异常当作未添加
  清理仅考虑自身有宿主归属证明且已满足保护条件的任务
  默认submit downloader.delete-task，保留文件；文件清理先submit downloader.prepare-cleanup
  下轮读取完成计划handle，授权允许且证据仍新鲜时再submit downloader.delete-files
```

额外在 `download.before` 注册同一策略 Gate，可对插件、手动、Subscribe 下载统一限制；任务计算与执行之间的额度变化由宿主事务内 reservation 防止超卖。插件 KV 只保存游标/展示缓存，不是剩余额度与任务所有权的事实来源。标签只能辅助展示，不能作为删任务的唯一依据。`delete-task` 完成后保留文件继续占用磁盘 quota；实际文件删除/确认不存在前不得释放占用。

Provider的size不是预算证据。宿主抓取并解析.torrent全部文件长度、校验溢出与内容摘要，以verified_total_bytes做最终同事务预留后才调用add。候选报1字节而metainfo为1TB时必须按1TB拒绝或预留；magnet元数据未可信取得时不自动刷流。不能通过插件申请的size调整掩盖实际文件大小。

文件清理由独立显式配置开启，要求 `command:downloader.delete-files` 和宿主实际持久 FileOperation：生成计划、确认具体文件归属及 Library 引用、记录 intent、执行、校验、落 receipt、可恢复重试。不存在“设置 delete_files=true 即自动进回收站”的承诺；没有此实现的宿主拒绝能力，刷流插件必须仍可仅删任务。HR/归属未知时拒绝清理，不猜测。

### 7.3 自定义 Indexer：API + Browser 搜索

声明 `{id:"example",kind:"private-pt",capabilities:["search","rss","detail","resolve-download","user-stats"]}`，Site 的 provider ref 为 `plugin:org.example.site/example`；宿主创建 Site 并保存凭据，插件只拿到当前 Site 的句柄与非秘密配置。内置 profile 和插件 provider 共用搜索入口、Filter、失败隔离及下载解析。

```text
search(site, query, page):
  校验 capability、Site scope；读取已验证的站点参数
  若 Site 为此 provider 显式启用 browser search：
    open(search) → navigate(search_path) → fill 搜索词 → click 提交 → wait 结果区域
    query 结果行，读取允许字段，构建 TorrentRaw
  否则 http.fetch(POST site-relative /api/search, body=query/page, auth_slot=site)
    校验 Content-Type、状态和正文上限，解析 JSON 为 TorrentRaw
  返回 batch(items,next_cursor,has_more)，不写 Subscribe，不提交下载
resolve-download(site, download_ref):
  验证 provider 自有引用，返回 site-relative 请求描述和宿主凭据槽位
  宿主检查来源、获取 .torrent 字节、验证并计算 info-hash，再运行下载服务
```

`TorrentRaw` 必填 provider-local ID、title、size、download_ref；可选 seeders/leechers/published/category/free/HR/IMDB/TMDB 等，每项有长度/范围限制。`site_id`、来源 plugin_id、generation、候选 handle 由宿主赋值；插件不能注册伪造的其他站点结果。`download_ref` 只回给同一 provider，不作为可访问 URL。

RSS 使用相同标准输出，默认 HTTP；浏览器路径必须在 descriptor 中明确声明并获得 `browser:search`。Torznab 的实例 host/port/path 与 API key 槽位由 Admin 在 Site 配置里确认，私网实例不扩大整个插件网络范围。失败按 SiteFailure 汇总，一个 Site 失败不吞掉其他 Site 的结果。

## 8. 开发、测试、打包与升级

### 8.1 推荐开发顺序

1. 先写 manifest、配置 schema 与最小权限，列出该插件的 public seam；用脱敏 fixtures 写失败测试。
2. 将策略、解析、成功判定写成纯函数，覆盖正常/空值/畸形/截断/未知字段；只在导出入口调用宿主能力。
3. 使用未来 SDK harness 注入 HTTP、Browser、读取视图、时钟和 command 收据，验证可观察输出与副作用，不断言私有函数或 mock 次数。
4. 用正式精确 WIT 生成组件，检查真实 imports、world 与 manifest；在沙箱实例执行资源/取消/越权测试。
5. 打包签名，在独立测试数据目录侧载；确认 Gate、unknown 对账、停用和更新代次正确，再发布。

计划提供的 CLI 如下，全部尚未实现；不是当前仓库可执行命令：

```sh
crawler-media plugin scaffold --world hook-plugin --api 1.0.0 ./traffic-policy
crawler-media plugin validate ./traffic-policy
crawler-media plugin test ./traffic-policy --offline
crawler-media plugin build ./traffic-policy
crawler-media plugin pack ./traffic-policy --output /tmp/traffic-policy.cmplugin
crawler-media plugin sign /tmp/traffic-policy.cmplugin --key /path/to/publisher.key
crawler-media plugin dev ./traffic-policy --instance http://127.0.0.1:18765
```

`dev` 必须仍走管理 API 的认证、开发模式、manifest 与权限检查，不能绕过签名政策或扩大网络；只热替换完整验证后的新 generation。签名私钥不进入仓库、不进入插件包、不进入宿主配置。实现时模板须锁定 Rust/WIT 工具链，并提供可复现构建记录与包 hash。

### 8.2 发布前行为测试矩阵

| 场景 | 必须观察到的行为 |
|---|---|
| F/G 输出越界字段、重复 handle、越权 Site | 拒绝该输出，保护输入与宿主约束，记录明确原因 |
| 手动/Subscribe/刷流添加同一 Torrent | 全部经过同一下载 Gate，幂等和 quota 相同 |
| command 被受理后插件崩溃/重启 | 原 operation 可查询，不用新 key 盲重发 |
| 添加外部任务成功但响应丢失 | Unknown 保持额度，先对账，不重复 add |
| 循环 A→B→A / 派生深度 5 | 重复 pair/深度限制生效，其他插件 Gate 未被绕过 |
| 配置修改、停用、授权撤销与在途调用竞争 | 固定 config_revision；旧 generation 无新写入，取消可观测 |
| 私网/重定向/子资源/WS/DNS rebinding | HTTP 和真实 Browser 连接都被拒绝；无法隔离则不启用 |
| 两个 Site 的 Browser 并发、取消、trap | Cookie/context 隔离，当前 context 被销毁，不影响别站 |
| 签到重复、challenge、200 错误页、提交后超时 | 不误报成功、不二次提交；只接受明确结果证据 |
| 刷流清理遇到外来任务/未知 HR/Library 引用 | 不删除任务或文件；给出可审阅的拒绝原因 |
| RSS/搜索解析或一个 Site 故障 | 其他 Site 结果仍返回，空结果与失败分别展示 |
| 删除 Library/取消 Job 后插件返回 | 拒绝过期 generation 的写入；远程 probe 仍固定并发 1、冷却 500ms |

普通 cargo 测试使用 fake Browser/HTTP，不访问外网、不启动 Chromium。真实浏览器网络隔离验收放在独立 E2E 环境，不能以 fake 测试代替；仓库 build/test 仍遵守单进程 `/tmp/cm-cargo.lock`、`TMPDIR=/tmp`、`-j8` 与 `--test-threads=8`。

### 8.3 安装、签名、升级和数据

发布物先计算整个 `.cmplugin` 的 SHA-256，再用已钉扎 Ed25519/minisign 发布者密钥签名；仓库索引提供 id/version/hash/download URL/key_id，宿主独立验包。首次信任由 Admin 明确导入/核验公钥，不把远程索引自报的新公钥自动当可信。密钥轮换需旧钥签署或 Admin 重新确认；撤销的包不再激活。

解包拒绝绝对路径、`..`、符号链接、重复路径、大小写碰撞与未声明可执行文件；限制条目数、单文件/总解压大小和压缩比。签名正确只证明发布者和完整性，不证明代码安全；权限、world 和出站策略仍强制执行。未签名包只能在 Admin 开启的开发模式侧载，关闭开发模式立即停用。

升级先并排校验新包、精确 API/world、权限差异、配置 schema 与 `state_schema`（即主设计 state-version），再进行明确激活。权限增加必须重新授予。state schema 相同且新配置验证通过可复用已有数据；state schema 不同则拒绝保留 KV 激活，必须由 Admin 导出后明确清空重置或取消升级。没有 `lifecycle.migrate`、自动 DB migration、旧字段 fallback 或静默清空 KV；不通过外置迁移工具形成另一套自动兼容流程。

激活顺序为先关闭旧代新准入、取消/drain在途调用并处理Unknown，再验证新组件，最后CAS切换active version/generation并发布新registry；不能先切换再声称旧代已经排空。旧command执行前再次验证代次。回退是对已验证旧包的显式再次激活，前提是其精确契约和数据schema仍满足；不宣称任意版本可无损自动回滚。卸载先撤销路由与任务，再处理在途操作和可选KV删除，不删除插件创建的真实下载/Library文件。

## 9. 与旧提案明确不同的地方

旧提案中的明文 `site.get-credentials`、任意 `site.update-session(cookie)`、同步业务写 import、任意下载器任务管理、虚构回收站、cron 已支持、双 MAJOR 兼容和自动迁移均不属于本契约。第三方插件不能执行 SQL/FS/raw CDP，也不能用 wasm 内的网络库绕开宿主。

Browser 搜索是本次需求新增的设计能力，必须随自定义 Indexer/Check-in 一起提供可验证实现；它扩展当前 ADR-0007 的使用范围，同时保留其 Cookie 隔离、opt-in 与维护请求防重复原则。是否已经实现，应以实施计划验收记录、正式 WIT/SDK 发布与测试证据为准。
