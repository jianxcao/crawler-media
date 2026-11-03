# 插件界面与管理 API

> 状态：未来设计，本文所有新增路由、DTO、页面与交互均待实现，不是当前 API 文档。
> 配套：[系统设计](../superpowers/specs/2026-11-03-plugin-system-design.md)、[编写接口](authoring.md)、[实施计划](../superpowers/plans/2026-11-03-plugin-system-plan.md)。
> 现行基线：`web/src/main.tsx`、`web/lib/permissions.ts`、`web/lib/http.ts`、`crates/api/src/http/routes/` 与 [self 合同](../api-contracts/self.md)。

## 1. 页面入口、身份与前端边界

设置「系统」分组增加「插件」。复用现有页面外壳、表单、确认对话框和可见页面轮询；不增加独立顶层导航。
插件管理、安装、运行、插件业务写、榜单管理、审计和 widget 默认仅 Admin。普通成员只在已有搜索/Subscribe 页面看到其获准业务的结果与精简说明。
后端仍按当前 User、业务能力和对象归属鉴权；插件 grant 不能扩大调用者可操作的对象。

| 路由 | 页面与主操作 | 门闸 |
|---|---|---|
| `/settings/plugins?tab=installed` | 已安装、状态、配置、启停；默认 tab | Admin |
| `/settings/plugins?tab=repos` | 仓库、公钥指纹、包目录、安装、更新 | Admin |
| `/settings/plugins?tab=developer` | 开发者模式、侧载包、诊断 | Admin + 必要时重新鉴权 |
| `/settings/plugins/:id?tab=overview` | 概览、版本、依赖对象、当前 operation | Admin；显式新增路由 |
| `/settings/plugins/:id?tab=config` | schema 表单、验证、保存 | Admin |
| `/settings/plugins/:id?tab=permissions` | 请求/已授予权限、对象 scope、撤销 | Admin |
| `/settings/plugins/:id?tab=tasks` | 定义、间隔、真实 Job 回执、手动运行 | Admin |
| `/settings/plugins/:id?tab=audit` | 审计、调用轨迹、错误与预算指标 | Admin |
| `/settings/sites` | 内置/插件 Indexer 分组、自定义 Site、Browser 授权 | 沿用 Site 管理权限 |
| `/subscriptions?tab=lists` | 榜单、待确认队列 | Admin 组件门闸 |
| `/list-subscriptions/:id` | 榜单规则、同步历史、来源条目 | Admin pathname 门闸 |
| `/search`、`/subscriptions/:id`、`/activity` | Hook 影响、来源、拒绝/延后/H&R 保护原因 | 沿用业务权限 |

`settings/plugins/:id` 比 `settings/:section` 更具体；插件 id 禁用 `installed/repos/developer/new` 保留字。
`settingsSectionGroupsFor` 隐藏成员入口，`accessiblePathFor` 同步阻止成员手输插件管理和榜单详情 pathname。
`AuthGate` 当前仅传 pathname；`?tab=lists` 必须在订阅组件内同步检查 `isAdmin`，不发榜单请求、不闪出内容，并 replace 回默认 tab。
成员会话权限变化时立即收起受限页并取消读取；后端 403 同样触发清理，不把前端隐藏当安全边界。
插件只能提供声明式 `stat/table/list/progress/chart` widget 与 schema；不执行插件 JS、HTML、iframe、远程脚本或自定义 CSS。
widget 的动作只能引用宿主已注册 action，链接只能引用宿主批准的内部路由；标签与错误文本按纯文本渲染。

## 2. 具体页面与完整交互

### 2.1 已安装、仓库与安装确认

```text
设置 / 插件                         [已安装] [仓库] [开发者]
[搜索插件________] [状态:全部 v]                   [添加插件]
名称 / 来源           版本       状态              最近任务        操作
PT 签到 / 已钉扎仓库  1.2.0      运行中            09:00 成功       [详情]
刷流 / 已钉扎仓库     2.0.1      等待在途退出      2 项正在结束     [查看操作]
自定义 Site / 侧载    0.3.0      需授权            未运行           [查看权限]
更新 1 项；异常 1 项                批量操作也逐插件展示 operation
```

首次无插件：说明“安装插件可扩展 Site、签到和工作流”，提供进入仓库按钮；不伪造推荐包或默认远程仓库。
搜索无匹配与尚未安装分开显示；加载失败保留上次列表并标“状态未刷新”，不变成空列表。
仓库卡显示 URL、公钥指纹、信任方式、上次同步时间、索引签名状态；首次信任必须显示独立可核对的完整指纹。
仓库刷新失败不覆盖最后一次验证通过的索引；缓存卡显示时间，无法据此安装被撤销或未验证的新包。
安装流程：选版本 → 获取准备结果 → 核对来源/包哈希/能力/对象 → 逐项确认新增敏感权限 → 提交 → 跟踪 operation → 配置 → 启用。
新安装默认停用；“安装成功”只说明包与登记完成，不表示配置可用、任务在跑或 Browser 可用。
升级也先准备，显示增删权限 diff、schema 差异、影响 Site/Subscribe/Job 数量；新增权限不沿用旧确认。
准备结果过期、revision 或包哈希变化须重新准备，不静默重签 token；旧版本处理以系统设计的升级原子性为准。

```text
安装 / 升级 PT 签到                 1.2.0 -> 1.3.0
来源 已钉扎仓库  指纹 SHA256:...    包 SHA256:...
保持  maintenance:provide           Site A、Site B
新增* browser:maintenance            仅 Site B
新增* http:site-auth                 仅 Site B 的批准 origin
[ ] 允许新增敏感权限；Browser 仍需实例与 Site 单独开启
受影响: 2 Sites / 1 Job；有效确认至 09:15:00
                              [取消] [确认并升级]
```

### 2.2 插件详情、停用与权限撤销

```text
< 插件   PT 签到  v1.3.0     已启用 / generation 7     [停用] [更多 v]
[概览] [配置] [权限] [任务] [审计]
当前操作：停用中 -> 等待在途退出    已结束 3 / 4       [查看详情]
能力：签到、Cookie 刷新   关联：Site A / Site B
健康：最近成功 09:00；最近错误 BrowserBusy；熔断 未触发
Browser：Site A 未选择；Site B 不支持网络隔离          [前往 Site 设置]
```

`enabled` 表达期望开关；运行态由宿主持久 lifecycle、generation 和 Job/command 回执计算，禁止从开关推断“运行中”。
显示状态至少包含：未配置、已停用、启用中、运行中、需授权、已熔断、停用中、等待在途退出、升级中、卸载中、受阻、失败。
停用受理后立即阻止新调用、排队 command 和新 Job；操作仍停留在等待状态，直到旧 generation 在途调用已取消/退出且回执收敛。
重新开启生成新 generation；旧 command 不因再次开启而复活。刷新页面仍能恢复同一 operation，不能靠组件计时宣布完成。
权限页按 capability 与精确 Site/Downloader/Library/Subscribe scope 展示“请求 / 已授予 / 实际可用”，敏感标记由后端返回。
权限目录包括 `hooks:<id>`、`events:<domain>`、`read:<domain>`、`command:<kind>`、`indexer:provide`、`maintenance:provide`、`list:provide`、`http:site-auth`、`browser:search`、`browser:maintenance`；具体闭集、ID 与参数以宿主目录为准。
撤销预览列出受影响的 Hook、任务与对象；确认后先关闭相应准入，再等待在途退出。未撤销的能力是否可继续由后端状态返回。
权限撤销可能立即阻断必要能力；界面提示“需重新授权”，不自动放宽 scope、改用匿名 HTTP 或替换其他 Site。
卸载确认列出关联 Site、榜单、已创建 Subscribe、刷流任务和插件私有数据；业务 Subscribe 保留，来源标为插件已卸载。
引用插件 Indexer 的 Site 保留并明确“提供者不可用”；不悄悄选内置 Indexer。活动下载的清理按归属/H&R策略另行处理。
关闭开发者模式也走异步操作：阻止新侧载、停用已加载的未受信任包，直到在途退出后显示完成。

### 2.3 配置、签到、刷流与自定义 Site

配置采用服务端返回的 JSON Schema 子集：object、string、integer、number、boolean、enum、array；默认拒绝未知字段。
允许 `minimum/maximum/minLength/maxLength/pattern/required` 和本地 `$defs`；不抓取远程 `$ref`、不执行表达式。
`x-ui.widget` 仅允许 `text/textarea/password/select/multi-select/site/downloader/library/subscribe/interval/bytes`；可见条件只比较当前字段常量。
只读 secret 返回 `{configured:true}`；保存用 `keep|replace|clear` 显式动作，空输入不代表删除。Site Cookie/API key 不存进插件表单副本。
“验证”只校验 schema、对象存在性、scope 与约束，不访问 Site；“测试连接 / 试运行”是独立有提示的异步任务。
保存成功更新管理 revision 与独立 config_revision；运行调用固定读取本次 config_revision。409 时保留草稿，展示服务端/草稿差异，由用户重新合并后提交，不能后台覆盖。

签到表单 schema 示例；`site` 选择器只列出 Admin 当前可授权的 Site，不把名称作为持久键：

```json
{
  "type": "object", "additionalProperties": false,
  "required": ["sites", "provider_id", "interval_secs", "transport"],
  "properties": {
    "sites": {"type": "array", "minItems": 1, "uniqueItems": true,
      "items": {"type": "string", "format": "uuid"}, "x-ui": {"widget": "site"}},
    "provider_id": {"type": "string", "enum": ["plugin:org.example.checkin/checkin"]},
    "interval_secs": {"type": "integer", "minimum": 3600,
      "maximum": 86400, "default": 86400, "x-ui": {"widget": "interval"}},
    "transport": {"type": "string", "enum": ["http", "browser"], "default": "http"}
  }
}
```

| 配置组 | 字段与交互 | 宿主强制边界 |
|---|---|---|
| Check-in | `sites[]`、`provider_id`、`interval_secs`、`transport:http/browser` | provider 必须已注册，Site 单独 opt-in；重试/退避受宿主控制 |
| 刷流范围 | `sites[]`、`downloader_id`、专用 `category`、候选 Filter | 仅操作自身持久归属任务；category 不是归属证明 |
| 刷流预算 | `budget_bytes`、`min_free_bytes`、`max_active_tasks` | 真实磁盘容量 + reservation 准入；按 Site/Downloader/插件任务记账 |
| 刷流上传 | `upload_target_bytes`、`upload_limit_bps`、`download_limit_bps` | 上传量目标与磁盘占用是独立数值，不能抵消容量 |
| 刷流保留 | `hold_days`、H&R 保护、`cleanup:delete-task/delete-files` | 默认 delete-task 保留文件；未知 H&R 视为受保护 |
| 自定义 Site | Site 名称、URL、`profile_id`、`provider_config`、凭证与代理 | URL、Indexer、权限 scope 均由宿主验证；凭证只在 Site 中 |
| Browser | 搜索 opt-in、维护 opt-in、可用性与精确 Site grant | 插件不能自行开启实例/Site开关或改 CDP 端点 |

```text
刷流 / 任务明细                         [配置] [暂停新添加] [运行一轮]
磁盘额度 200 GiB   已占用 122 GiB   已预留 35 GiB   可准入 43 GiB
上传目标 1 TiB    已上传 680 GiB   上传限速 10 MiB/s
Torrent / Site   Downloader归属   H&R/保留       Job回执      下一动作
条目甲 / A       owned / job-...  保护至11-05    做种中       保留
条目乙 / B       owned / job-...  状态未知       检查失败     受阻
条目丙 / A       owned / job-...  已满足         删除任务成功 文件仍占用
```

刷流一轮必须展示“发现 → 规则过滤 → 容量预留 → 添加 → 状态对账 → 保留/保护 → 清理 → 释放预留”的实际回执。
失败后保留 reservation 待对账，不能按 RPC 超时猜测未添加；Downloader 不可用显示容量未知并暂停新准入。
删除任务成功不会假设文件已释放；已保留文件继续计入占用，只有持久文件操作的实际完成回执才能确认释放。
`delete-files` 不称为回收站，不提供虚假的撤销；需精确文件清单、文件身份/版本、归属、H&R/hold 证据和 Admin 显式授权。
任一文件有 Library/其他任务引用或归属不确定就阻止该清理；插件不能绕过宿主文件操作直接删除。
Check-in 页显示每 Site 的上次成功、已签到、Cookie 过期、Browser 排队、错误阶段与下次执行；不会把“插件开启”当签到成功。
当前 `Schedule` 只有 Interval；表单用秒数与友好的间隔选择，不提供已支持 cron 的文案或 `format:cron`。
自定义 Site 创建流程：选插件 Indexer → 填宿主 Site 字段 → 填 provider schema → 授权对象 → 可选验证 → 保存。
提供者不可用时可编辑 Site 宿主字段并更换提供者；切换必须明确确认 schema 不同，不能把原配置套进新提供者。

### 2.4 Browser、搜索与 Subscribe 中间步骤

本方案允许插件通过受控 Browser 进行搜索及签到，是对现行 ADR-0007“搜索仅 HTTP”的提议扩展；落地前以 ADR-0013 为准。
Browser 可执行需同时满足：实例开关、该 Site 对具体用途 opt-in、`browser:search` 或 `browser:maintenance` grant、可验证网络隔离能力。
页面状态：未配置、实例关闭、Site 未选择、需授权、不支持隔离、就绪、忙、会话过期；显示具体修复入口，不展示 CDP URL、Cookie 或调试 token。
Browser 诊断只返回安全能力摘要；未满足隔离不能运行插件页面导航，不能用普通 BrowserContext 创建成功冒充网络隔离成功。
签到导航后超时显示“结果待确认”，等待宿主对账，不立即改走 HTTP 再签到；是否允许导航前降级以 provider 合同与宿主策略为准。

```text
PT 搜索 [关键词_________] [Sites v] [Filter v]                   [搜索]
Site A：HTTP 完成 20条    Site B：Browser 排队    Site C：提供者不可用
插件处理：移除 3条 / 调整排序 5条                                [查看说明]
Torrent         Site   核心Filter   最终排序   处理原因
条目甲          A      通过        1          插件提升；原序号4
条目乙          A      拒绝        --         H&R未知（不可被插件恢复）
```

搜索按 Site 展示成功/失败/待处理，不把单 Site Browser 失败当全局失败；新查询取消前一个查询的订阅与展示。
Hook 轨迹包含输入版本、插件顺序、排除原因、调分/重排前后、耗时、降级或阻断原因；凭证、原始页面和无权对象不进入轨迹。
插件不得恢复被核心安全/身份/权限检查淘汰的 Torrent；重排只作用于宿主准入集合，并保持稳定 tie-break。
Subscribe 详情沿现有流水线增加“插件处理”行：查询调整、候选过滤、排序、投递前校验、延后/拒绝、执行回执。
Site 登录/Check-in 的中间 Hook 显示 transport 决策、执行前 veto、结果观察；不能由观察事件伪造成功或覆盖凭证。
普通成员只读其操作相关的原因摘要；Admin 可展开完整审计并跳转插件，成员不通过轨迹获得站点凭证或其他用户记录。
榜单页以来源条目展示待确认/已创建/已存在/忽略/失败；创建 Subscribe 的 owner、Filter、Library 必须明确，插件不能替用户选择越权对象。

## 3. DTO、并发与异步操作合同

所有管理 API 使用同源 `/api/v1`；成功 `{ok:true,data}`，失败 `{ok:false,error:{code,message}}`，时间为 RFC3339 UTC，空值为 null。
领域对象 ID 用 UUID 字符串；`plugin_id` 是 manifest 的稳定命名标识，task/provider/hook 是宿主验证的命名 ID，不能转数字。
列表使用 `cursor/limit`（默认50、最大200），信封顶层 `next_cursor`，`data` 为数组；不引入 offset。
`web/lib/http.ts::request<T>` 返回完整信封，API adapter 明确读取 `.data`，不假定 request 已解包；错误从 `HttpError.details` 读取。

| DTO | 必需字段与含义 |
|---|---|
| Plugin | `id,name,version,package_hash,source,revision,config_revision,generation,enabled,lifecycle,health,required_grants,effective_grants,active_operation_id` |
| Grant | `capability,scope,sensitive,status`；scope 是精确对象集合，新增对象不会自动获得授权 |
| Preparation | `preparation_id,plugin_id,package_hash,base_revision,grant_diff,config_diff,impact,consent_token,expires_at` |
| Operation | `operation_id,plugin_id?,kind,state,phase,revision,generation?,job_id?,progress,blocked_reason,error,result,created_at,updated_at` |
| Task | `id,plugin_id,definition_revision,enabled,schedule:{interval_secs},last_job,next_run_after,ownership_summary` |
| Receipt | `command_id,operation_id,job_id,scope,generation,state,effect,evidence,observed_at`；可含脱敏的归属/文件证据引用 |
| Invocation | `id,hook_id,workflow_id,input_revision,plugin_steps[],decision,reason,duration_ms`；步骤仅含获准投影 |
| BrowserAvailability | `site_id,purpose,status,missing_requirements[],retry_after_ms?`，不返回端点、cookie、原始 CDP 信息 |
| FieldError | `path,code,message`；配置校验失败放 `error.fields`，全局错误仍有 code/message |

资源 GET 返回 `revision` 与 ETag；修改已有资源必须 `If-Match: "<revision>"`，缺失428，过期409，不自动合并。
创建以调用方 UUID 或包内 plugin_id 确定身份，用 `If-Match: "0"` 表示不存在；重复身份409，不能把 POST 当 upsert。
所有写请求携带 `Idempotency-Key`：同主体、同方法/规范路径/同 body 返回原受理结果；同 key 不同请求体409。
幂等凭证与结果持久化；命中幂等记录先重验身份权限，再返回原 operation，不用重放时已经过期的旧 If-Match 创建第二次执行。
持久记录保存规范请求摘要及脱敏结果，不把密码、API key、确认 token 或明文 secret存进一般operation与审计。Endpoint key由宿主Web界面用安全随机源生成并先提供复制，创建接口只保存校验摘要且只返回元数据，因此超时重发无需再次回显secret。
确认 token 绑定 Admin、包哈希、当前 revision、grant 摘要、精确 scope 与短期 expiry；不能替代最终 CAS 与权限复查。
启停、安装、升级、卸载、权限撤销、手动任务与插件业务写均 POST，受理返回202 `{operation_id,state}` 与 `Location`。
operation 权威在 app.db；jobs.db 负责执行及回执，两库不承诺原子提交。只有持久 operation 受理才返回202，恢复机制补齐未入队工作。
状态为 `queued|running|waiting|blocked|succeeded|rejected|deferred|failed|cancelled|unknown`，与组件OperationView一致；phase 表示校验、安装、排空、切换等细分步骤。deferred包含next_run_at；unknown只允许对账或显式人工处理，不自动重放副作用。
`blocked` 不是成功且不自动终结；例如 H&R 未满足、Browser 隔离缺失，保留可解释原因与允许的 resume/cancel 动作。
只有 `succeeded|rejected|failed|cancelled` 是终态；cancel 是请求停止，不能反转已发生的外部副作用或文件删除，未确认效果的操作仍保留unknown。
GET operation 纯读不推进工作；轮询页面可见时进行，按服务端 `retry_after_ms`，终态停止；页面切换不等于取消服务端操作。
客户端15秒超时或断网记“提交结果未知”；保留 idempotency key，按已知 operation 轮询或以同 key 重取原受理结果，不换 key 盲重发。
权限撤销/CAS检查在受理与实际执行准入时均复查；所有写沿用实例只读、维护门禁及目标库 writer 最小临界区。

## 4. 管理 REST 明细（全部待实现）

下表路径省略 `/api/v1`。`A`=Admin；`A+R`=Admin 且重新鉴权 token；`M`=现有业务权限和对象归属；所有写含上节幂等头，CAS指目标 revision。
重新鉴权token绑定当前登录主体、purpose与短期expiry：写请求用reauth_token字段，GET导出下载用`X-Plugin-Reauth`头，不放URL/query。HTTP日志必须遮盖此头。
`202 Op` 表示受理 operation 引用，`200 T`/`201 T` 均包在标准信封；只读 POST 验证单独进入只读路由组，不能执行插件或联网。

| 方法与路径 | 输入 | 输出 | 权限 / CAS |
|---|---|---|---|
| GET `/plugins` | `state?,cursor?,limit?` | 200 Plugin[] | A |
| GET `/plugins/:id` | 无 | 200 Plugin + 关联对象摘要 | A |
| GET `/plugins/:id/config-schema` | 无 | 200 schema、schema_revision | A |
| GET `/plugins/:id/config` | 无 | 200 values、secret状态、revision/config_revision | A |
| POST `/plugins/:id/config/validate` | `schema_revision,values` | 200 校验结果/400 FieldError[] | A；只读 |
| PUT `/plugins/:id/config` | `schema_revision,values,secrets` | 200 脱敏配置与新revision | A / Plugin |
| GET `/plugins/:id/permissions` | 无 | 200 requested/effective Grant[]、revision | A |
| POST `/plugins/:id/permissions/prepare` | `requested_grants[]` | 200 Preparation | A；只读准备 |
| POST `/plugins/:id/permissions/grant` | `consent_token,grants[]` | 202 Op | A / Plugin |
| POST `/plugins/:id/permissions/revoke` | `grants[],reason` | 202 Op | A / Plugin |
| POST `/plugins/:id/enable` | `{}` | 202 Op | A / Plugin |
| POST `/plugins/:id/disable` | `reason?` | 202 Op | A / Plugin |
| POST `/plugins/:id/uninstall` | `remove_private_data:bool` | 202 Op；业务对象保留 | A / Plugin |
| POST `/plugin-installations/prepare` | `repo_id,plugin_id,version` 或 `upload_id` | 202 Op；result=Preparation | A / 新建0 |
| POST `/plugin-installations` | `preparation_id,consent_token,grants[]` | 202 Op；完成后停用 | A / 新建0 |
| POST `/plugins/:id/upgrades/prepare` | `version,repo_id?或upload_id,target_config,state_policy:preserve/reset` | 202 Op；result=Preparation含state/config差异与digest | A / Plugin |
| POST `/plugins/:id/upgrade` | `preparation_id,consent_token,grants[],state_policy,target_config_digest`；reset另含reauth_token | 202 Op | A；reset时A+R / Plugin |
| POST `/plugins/:id/rollback` | `package_hash,preparation_id,consent_token,state_policy,target_config_digest` | 202 Op；同样重新校验权限/状态，reset需reauth | A或A+R / Plugin |
| POST `/plugins/:id/state/exports` | `security_scopes[],include_blobs:bool,reauth_token` | 202 Op；result只含export_id/expiry，不含数据 | A+R / Plugin；仅disabled且drain完成 |
| GET `/plugin-operations/:id/export` | 无 | 200 authenticated附件流，含包digest/state_schema/配置revision/各scope数据 | A+R；仅原导出主体，默认10分钟有效，Cache-Control:no-store |
| POST `/plugins/:id/state/reset/prepare` | `security_scopes[]` | 200 清除数量/字节、state revision、短期consent_token | A；只读 |
| POST `/plugins/:id/state/reset` | `security_scopes[],consent_token,reauth_token` | 202 Op；只清明确选中的KV/blob，配置另行修改 | A+R / Plugin；仅disabled且drain完成 |
| GET `/plugin-operations/:id` | 无 | 200 Operation | A |
| POST `/plugin-operations/:id/cancel` | `reason?` | 202 同一 Op | A / Operation |
| POST `/plugin-operations/:id/resume` | `evidence_ref?` | 202 同一 Op；不重复已有副作用 | A / Operation |
| GET `/plugins/:id/tasks` | `cursor?,limit?` | 200 Task[] | A |
| POST `/plugins/:id/tasks/:taskId/update` | `enabled,schedule:{interval_secs},config` | 202 Op；app期望配置与Jobs独立同步 | A / Task |
| POST `/plugins/:id/tasks/:taskId/run` | `input` | 202 Op；关联Job | A / Task |
| GET `/plugins/:id/tasks/:taskId/receipts` | `cursor?,limit?` | 200 Receipt[] | A |
| GET `/plugins/:id/brush-state` | `site_id?,downloader_id?` | 200 占用/预留/上传/归属/保护快照 | A |
| GET `/plugins/:id/brush-cleanup-preview` | `task_ids[]` | 200 精确文件/保护/引用证据与短期确认token | A |
| POST `/plugins/:id/commands` | 注册的 `kind,payload,evidence_ref?` | 202 Op；拒绝任意RPC | A / Plugin |
| GET `/plugins/:id/audit` | `action?,from?,cursor?,limit?` | 200 脱敏审计[] | A |
| GET `/plugins/:id/metrics` | `window_secs` | 200 延迟/错误/预算/熔断快照 | A |
| GET `/plugins/:id/widgets` | 无 | 200 声明式widget[] | A |
| GET `/plugin-hook-catalog` | 无 | 200 已注册Hook合同/失败策略 | A |
| GET `/plugin-invocations/:id` | 无 | 200 Invocation | A；成员使用业务摘要 |
| GET `/plugin-repos` | `cursor?,limit?` | 200 仓库[] | A |
| GET `/plugin-trust-keys` | 无 | 200 key_id、公钥指纹、状态、关联包数 | A |
| POST `/plugin-trust-keys/prepare` | `public_key,label` | 200 规范公钥/计算指纹/短期consent_token；不联网 | A；只读 |
| POST `/plugin-trust-keys` | `public_key,fingerprint,consent_token,reauth_token` | 201 key元数据；可用于离线包验证，无需repo | A+R / 新建0 |
| POST `/plugin-trust-keys/:keyId/revoke` | `reason,reauth_token` | 202 Op；阻止新激活并drain该key相关插件 | A+R / Key |
| GET `/plugin-repos/:id` | 无 | 200 仓库、revision、待核验指纹 | A |
| POST `/plugin-repos` | `id,url,pinned_key_fingerprint` | 201 仓库 | A / 新建0 |
| PATCH `/plugin-repos/:id` | `enabled?,url?`；URL变化须重新信任 | 200 仓库 | A / Repo |
| POST `/plugin-repos/:id/trust/prepare` | 已抓取候选的 `fingerprint` | 200 差异、短期confirmation_token | A；只读，不联网 |
| POST `/plugin-repos/:id/trust` | `fingerprint,confirmation_token` | 200 仓库 | A+R / Repo |
| POST `/plugin-repos/:id/refresh` | `{}` | 202 Op | A / Repo |
| GET `/plugin-repos/:id/index` | `cursor?,limit?` | 200 已验证包条目[]、验证时间 | A |
| DELETE `/plugin-repos/:id` | 无 | 200 `{removed:true}`；已装包保留来源 | A / Repo |
| GET `/settings/plugins` | 无 | 200 开发者模式/策略/revision | A |
| POST `/plugin-reauth` | `password,purpose` | 200 短期且绑定主体/purpose的reauth_token | A；认证不修改资源revision |
| POST `/settings/plugins/developer/enable` | `reauth_token` | 200 设置 | A+R / Settings |
| POST `/settings/plugins/developer/disable` | `{}` | 202 Op | A / Settings |
| POST `/plugin-uploads` | multipart `upload_id,file:.cmplugin,signature:.minisig?`，摘要头 | 201 `upload_id,hash,signature_status,expires_at` | A+R / 新建0；正常模式只收已信任签名包，未签名仅开发模式 |
| GET `/plugins/:id/browser-availability` | `site_id,purpose:search/maintenance` | 200 BrowserAvailability | A |
| POST `/plugins/:id/browser-checks` | `site_id,purpose` | 202 Op；不导航Site | A / Plugin |
| GET `/plugins/:id/endpoint-keys` | 无 | 200 key元数据；不回显secret | A |
| POST `/plugins/:id/endpoint-keys` | `id,secret,endpoint_ids[],capabilities[],object_scope,expires_at`；issuer_user_id由当前Admin固定 | 201 委托key元数据；secret不回显 | A+R / 新建0；默认能力空集只读状态，不默认授权业务数据 |
| DELETE `/plugins/:id/endpoint-keys/:keyId` | 无 | 200 `{revoked:true}`；立即挡新请求 | A / Key |

权限准备只计算已有 schema/grant 差异，不获取远程包；下载/签名校验的安装或升级准备以异步 operation 执行。
离线签名包通过同一upload上传包外detached signature，随后prepare核验digest与本地已钉扎公钥；不能因没有仓库URL要求开启开发模式。YAML-only包也必须遵守签名/来源/命名空间/权限检查。
尚无信任公钥时先调用trust-keys/prepare，界面展示由实际公钥计算的完整指纹，再经重新鉴权确认导入；不信任包/远端索引自报的公钥。撤销key是可观察异步操作，默认拒绝该key包的新调用并排空，不悄悄替换为新key。
回滚使用已有版本调用升级准备接口，必须重新校验当前信任、包完整性和权限；没有“跳过确认的本地回退”。
配置 PUT 仅在插件已停用且drain完成时保存经过宿主schema验证的配置并提升revision；其他状态返回409并提示先停用，不暗中在同步请求运行插件。下次启用时才执行组件lifecycle.validate，失败仍保持停用。
state_schema改变时升级prepare必须选择reset并提交完整target_config；preserve只适用于相同state_schema。准备结果明确会清除哪些scope/数量/字节，先提供导出入口；确认token同时绑定package hash、当前revision、target_config digest、state_policy和重置范围。缺任何字段或摘要改变都拒绝激活，不能用一次普通升级确认暗含清空。
导出在锁外分块写受保护临时artifact，快照按短read scope有界读取；插件停用及state revision fence保证导出期间无新KV写。下载仍须当前Admin权限和同一重新鉴权主体，不签发public URL；到期关闭handle并清理临时文件。导出可能包含插件曾保存的认证页面，界面标明敏感。此导出用于备份/检查，不提供跨state_schema自动导入。单独reset不删除Site凭据、维护receipt、下载归属、用户Subscribe或审计；升级reset与active指针采用同一app持久operation，崩溃后不能呈现“新代码+未重置旧state”。

| 业务扩展方法与路径 | 输入 | 输出 | 权限 / CAS |
|---|---|---|---|
| GET `/sites/catalog`（扩展） | 现有参数 | 新增 `source,plugin_id,provider_id,availability` | M |
| POST `/sites`（扩展） | 现有Site字段 + `provider_config`、Admin明确选择Browser用途 | 现有Site信封；凭证遮盖 | A；现有Site合同 |
| GET `/sites/:id/plugin-status` | 无 | provider/Hook/Browser可用摘要 | A |
| POST `/sites/:id/plugin-actions` | `action:login/checkin/verify,provider_id` | 202 Op | A / Site |
| GET `/search/torrents`（扩展） | 现有搜索参数 | 新增安全的处理原因与调用引用 | M；既有查询鉴权 |
| GET `/subscriptions/:id/activities`（扩展） | 现有参数 | 新增Hook阶段/原因/来源；成员脱敏 | M；归属 |
| GET `/list-subscriptions` | `cursor?,limit?` | 200 榜单[] | A |
| GET `/list-subscriptions/:id` | 无 | 200 榜单与规则/revision | A |
| POST `/list-subscriptions` | `id,provider_id,source,owner_id,rule,schedule` | 202 Op | A / 新建0 |
| POST `/list-subscriptions/:id/update` | `rule?,schedule?,enabled?` | 202 Op | A / List |
| POST `/list-subscriptions/:id/remove` | `{}`；已建Subscribe保留 | 202 Op | A / List |
| POST `/list-subscriptions/:id/sync` | `{}` | 202 Op | A / List |
| GET `/list-subscriptions/:id/items` | `state?,cursor?,limit?` | 200 来源条目/创建结果[] | A |
| POST `/list-subscriptions/:id/items/:itemId/accept` | `owner_id,filter_id,library_id` | 202 Op；result=Subscribe引用 | A / Item |
| POST `/list-subscriptions/:id/items/:itemId/ignore` | `reason?` | 202 Op | A / Item |
| POST `/list-subscriptions/:id/items/:itemId/allow` | `{}`；只重新允许，不立即创建 | 202 Op | A / Item |

### 4.1 插件自定义端点

`/plugins/:id/endpoints/*` 只注册 manifest 声明的方法/路径；认证模式仅 `user|admin|apikey`，禁止匿名 `public`。
`user` 仍受当前 User 能力/对象归属限制；`admin` 必须 Admin；`apikey` 仅认 `Authorization: Bearer <key>`，禁止 query、cookie、body传key。
key持久绑定插件、精确endpoint集合、issuer_user_id、capabilities、object_scope、到期时间，派生独立`EndpointKey{key_id,issuer_user_id}`主体；不是全实例API key。capabilities只接受该endpoint声明的read/command子集，object_scope按Site/Downloader/Library/Subscribe精确ID分别授权；空集不等于所有对象，禁止通过key创建自身grants。插件不能读取key明文，日志遮盖认证头。
只读端点可返回受限 `application/json` 或 `text/plain` 自定义响应；这类响应不走 `request<T>` 管理信封客户端，也不在 UI 内嵌。
会产生业务副作用的端点只受理宿主 command，返回标准202 operation信封；不允许同步执行后伪装成只读响应。
默认业务写仍需Admin；user不隐含写权限，apikey仅在Admin显式委托该command与对象范围时可写。有效权限取key委托∩当前issuer权限∩插件grants∩endpoint声明，issuer被停用/删除/降权后立即收紧，不能保存一份永久Admin权限快照。
`GET /plugins/:id/endpoint-operations/:operationId`用原endpoint鉴权模式提供该调用者创建的操作摘要；key请求必须匹配原key_id+issuer主体，不能因同一签发者读取另一个key的操作；撤销/到期/轮换后旧key拒绝轮询，Admin可从管理入口处理遗留操作。
宿主拒绝 `text/html`、JavaScript MIME、`Set-Cookie`、站外 `Location`；设置 `nosniff`，限制大小/耗时/方法，不转发任意响应头。
原始输入不能直接转成有更高权限的 command；服务端重建 User/插件/grant/object scope后鉴权，并保留 caller与plugin双重审计。

## 5. 请求与响应样例

### 5.1 升级确认后的受理与进度

```http
POST /api/v1/plugins/org.example.checkin/upgrade
If-Match: "12"
Idempotency-Key: 1f87a4d3-9138-43ee-bb15-512758b794d7
Content-Type: application/json

{"preparation_id":"8ced2998-ff2b-45ac-811b-9bd800ee7291","consent_token":"opaque-short-lived-token","state_policy":"preserve","target_config_digest":"sha256:validated-config-digest","grants":[{"capability":"browser:maintenance","scope":{"site_ids":["7d2c4008-c814-4511-b940-a05fd07fca09"]}}]}
```

```json
{"ok":true,"data":{"operation_id":"83a6507e-36b6-4ad0-b5f2-0c6bcb040780","state":"queued"}}
```

`GET /api/v1/plugin-operations/83a6507e-36b6-4ad0-b5f2-0c6bcb040780` 的非终态示例：

```json
{"ok":true,"data":{"operation_id":"83a6507e-36b6-4ad0-b5f2-0c6bcb040780","plugin_id":"org.example.checkin","kind":"upgrade","state":"waiting","phase":"draining","revision":3,"generation":7,"job_id":null,"progress":{"finished":3,"total":4},"blocked_reason":null,"error":null,"result":null,"retry_after_ms":1000,"created_at":"2026-11-03T01:00:00Z","updated_at":"2026-11-03T01:00:02Z"}}
```

### 5.2 保存签到配置与字段错误

```http
PUT /api/v1/plugins/org.example.checkin/config
If-Match: "13"
Idempotency-Key: 62658b65-f7b8-414b-934d-c445192270b9
Content-Type: application/json

{"schema_revision":2,"values":{"sites":["7d2c4008-c814-4511-b940-a05fd07fca09"],"provider_id":"plugin:org.example.checkin/checkin","interval_secs":86400,"transport":"browser"},"secrets":{}}
```

```json
{"ok":true,"data":{"revision":14,"config_revision":3,"schema_revision":2,"values":{"sites":["7d2c4008-c814-4511-b940-a05fd07fca09"],"provider_id":"plugin:org.example.checkin/checkin","interval_secs":86400,"transport":"browser"},"secrets":{}}}
```

同一 schema 的非法间隔返回400，不自动改成默认间隔：

```json
{"ok":false,"error":{"code":"plugin.config_invalid","message":"配置校验未通过","fields":[{"path":"/values/interval_secs","code":"minimum","message":"执行间隔必须至少为3600秒"}]}}
```

### 5.3 手动刷流任务：接单不等于添加成功

```http
POST /api/v1/plugins/org.example.brush-budget/tasks/reconcile/run
If-Match: "4"
Idempotency-Key: 90638d1d-7afc-4e89-9b0f-54861c9990e1
Content-Type: application/json

{"input":{"site_ids":["7d2c4008-c814-4511-b940-a05fd07fca09"],"mode":"reconcile"}}
```

```json
{"ok":true,"data":{"operation_id":"f435bf77-7603-4ec4-a4aa-b36cda5c4edb","state":"queued"}}
```

### 5.4 错误码与 UI 行为

| HTTP / code | UI 行为 |
|---|---|
| 401 `auth.required` | 沿用登录跳转；保存操作ID与非敏感草稿，不存密码/secret |
| 403 `plugin.forbidden` / `plugin.grant_revoked` | 关闭受限入口/动作，重取会话或权限；不重试业务写 |
| 404 `plugin.missing` / `plugin.operation_missing` | 显示对象不存在；未知operation不视为失败或已完成 |
| 409 `plugin.revision_conflict` | 保留草稿并重新加载；显示当前revision，不自动覆盖 |
| 409 `plugin.idempotency_conflict` | 提示请求标识已用于另一内容；禁止自动换key重发 |
| 409 `plugin.consent_stale` | 重新准备并展示权限diff；之前勾选失效 |
| 409 `plugin.operation_active` | 跳转现有operation；不重复启停/升级 |
| 409 `instance.read_only` | 显示实例只读，保留草稿；不排队写入 |
| 428 `plugin.precondition_required` | 重新读取revision后由用户重新提交；属于客户端合同错误 |
| 400 `plugin.config_invalid` / `plugin.scope_invalid` | 定位字段与对象，保留其他输入 |
| 422 `plugin.browser_isolation_unavailable` | 显示“不支持隔离”，链接诊断；不自动切换不隔离Browser |
| 429 `plugin.budget_exceeded` | 展示额度/冷却，遵守Retry-After；业务写需人工确认再提交 |
| 503 `runtime.busy` / `plugin.capacity_unavailable` | 展示维护/排队容量原因；未受理不能报已排队 |
| 网络中断 / status 0 | 显示“结果未知，正在确认”；保留并复用原idempotency key |

## 6. UI 与 API 验证清单

以下是实施验收，不宣称本轮已通过；测试以公开HTTP/可见页面行为为 seam，外部Site/Downloader/Browser用fake，不启动Chromium。

1. Admin/成员直达所有路由、query tab与浏览器前进后退；成员不闪出插件页，不请求榜单API，无权对象返回403/404。
2. request adapter按完整信封解包；201/202/error.fields与顶层cursor契约均覆盖，不引入另一个自动解包客户端。
3. 安装/升级展示真实包hash与权限diff；新权限、scope、revision、expiry任一变化使旧token失效，确认不能复用到另一包。
4. 双击、刷新、请求超时和并发标签页仅生成一个operation；相同key不同body409；缺少If-Match428，旧revision409且草稿保留。
5. 停用/撤销/升级期间显示等待与受阻；旧generation command不复活，重启后同operation继续，终态前不出现成功toast。
6. 配置secret遮盖、keep/clear区别、未知字段、非法schema与远程引用；恶意widget标签/链接/自定义端点不能执行脚本或设置cookie。
7. Browser四重条件逐一缺失、忙、会话过期、导航后超时；UI不展示CDP地址或Cookie、不盲目切HTTP重复签到。
8. PT搜索每Site独立结果、插件拒绝/重排轨迹、核心淘汰不可恢复、Subscribe中间步骤延后/拒绝可见且成员摘要脱敏。
9. 刷流完整闭环：额度不足、并发reservation、添加结果未知、重启对账、H&R/hold、文件保留占用、无证据删除被拒绝。
10. 卸载后Subscribe仍在、插件Site明确不可用、榜单来源保留；清理私有数据不会把业务记录或下载文件一并删除。
11. 空列表、过滤无结果、过期缓存、401/403/409/428/503、离线恢复、键盘操作、焦点返回、窄屏表格与长中文错误均可读。
12. 写门禁在真正写入前复查；GET轮询不拿writer、不推进operation；读失败不被渲染成空成功。
