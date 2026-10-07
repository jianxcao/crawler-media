# Roadmap: bug 修复 + 自有 API 重构 + 全模块功能实现

> **本文是 2026-09 起的阶段档案，不是现行接口规范。**
>
> - 领域词汇：`CONTEXT.md`
> - API 信封与核心 DTO：`docs/api-contracts/self.md`（约定，不是完整路由表）
> - 现行路由：`crates/api/src/http/mod.rs`、`web/lib/api/*`
>
> 文中打 ✅ 的条目记录当时落地的方向；之后若端点合并或改名（例如
> `/tracking-state` 并进 `PATCH /subscriptions/{id}`），以代码为准，不要按
> 本文去「补回」旧路径。

来源：2026-09 浏览器 E2E 测试 + parity 差距盘点 + 用户方向决策。

## 方向决策（用户指令，覆盖此前兼容路线）

- **不做任何 API 兼容**：删除上游兼容层（`ApiResponse` 信封、DTO 形状、`numeric_id` hash、/api/v1 兼容路由组）。后端按我们的领域模型设计**自有 REST API**，永远走最优解。
- **界面类似上游**：保留 `apps/mc-web` 作为视觉基底（silver-glass 主题、海报墙、订阅卡片、播放器），前端 `lib/api/*` 与组件层**大改以适配自有 API**；上游专属交互（AI 对话、插件、转码、IM 通道、liquid-glass discover 等）删除或剥离。
- **功能全部要**（除 AI）：Media 识别、Sites/Indexer、Filter、Subscribe（含 Wash-cut/赛季深度）、Downloader、Transfer/入库、Library/媒体库、Scrape、Playback（Jellyfin 直连）、Jobs、Notify（走 jianxcao/notify）、刷流（按上游现有方式实现，后期可改）。
- **明确不做**：AI（agent/llm/mcp）、插件模块（含浏览器扩展，issue #129 挂起）、转码/字幕生成/PGS OCR/Photos/MCP server/OTA、IM 通道（weixin/telegram/discord/webhook 版）。通知唯一出口 = `jianxcao/notify`。
- **删除范围**：明确不做功能的端点与前端入口删干净；**stub 尽量真实化**，不删有数据来源的 stub。

## 阶段 0 —— 已发现 bug 修复（✅ 完成，commit "Fix transfer path mapping, title normalization, and duplicate transfer"）

| # | 级别 | 问题 | 状态 |
|---|---|---|---|
| B2 | P0 | Transfer 路径映射缺失：qB save_path `/downloads` 宿主不可见 | ✅ QbitDownloader/Transmission 支持 path_maps（`CRAWLER_MEDIA_QB_PATH_MAP` / `CRAWLER_MEDIA_TR_PATH_MAP`），live-loop 并入 |
| B6 | P2 | watch_ledger 标题带连字符与订阅 media 匹配不上 | ✅ release::parse 不再把 `-` 当 title token |
| B9 | P0 | Transfer 成功后 pending 未清理 → 重复转移/副本文件 | ✅ RunOutcome.transferred_enclosures + delete_pending + facts 已录跳过 |
| B8 | P3 | 编译警告 | 随阶段顺手清理（部分完成） |

## 阶段 1 —— 自有 API 重构（核心，最大块）

- A1. ✅ **API 契约设计**：`docs/api-contracts/self.md` 完成。
- A2/A3. ✅ **后端**：删除上游兼容层（webapi DTO/信封/numeric_id），新 `crates/api/src/http/` 模块实现全部资源；store 加 downloaders.path_maps / users.role 列；CLI 与 management 测试适配 UUID/新信封。commit "Self-hosted REST API: replace upstream compatibility layer"。
- A4. ✅ **前端 lib/api 重写**：全部按自有契约重写（UUID string id、{ok,data} 信封），删除 AI/插件/转码/IM 模块。
- A5. ✅ **前端组件适配**：组件改调新 lib；删除 AI/IM/转码/插件外围（sessions/new/people/setup 路由、settings 外围分区、account-switcher、share、collection、photo 降级等）；保留媒体库/订阅/搜索/活动/设置核心。`next build` 19 路由全过 + `tsc --noEmit` 0 错误 + 浏览器 E2E 验证（登录/媒体库/订阅/活动/设置/搜索均正常，UUID id、真实 stats、无白屏崩溃）。

## 阶段 2 —— 订阅深度（✅ 完成）

- D1. `/subscriptions/{id}/tracking-state`、`/follow-future`：状态落 subscribe 行。
- D2. `/missing-resource-searches`：触发补缺搜索 job。
- D3. `/upgrade-runs`：基于 subscribe facts（wash-cut/quality）发起升级轮。
- D4. `/removal-preview` + `/season-cleanup` + `DELETE`（异步清理 job，参照 wash-cut 删除语义）。
- D5. `/activities` + `/today-arrivals`：从 jobs/facts 推导，不再空数组。
- D6. `/search/history` + presets 持久化。

## 阶段 3 —— 媒体库深度（✅ 完成）

- L1. 条目详情 + episodes 列表（修复 B4 的完整版）。
- L2. `/scan`、`/metadata/refresh`（catalog fanout 拉真实元数据 + 海报）。
- L3. `missing`、`item-index`、`facets`、`gallery`。
- L4. duplicates / trash / identification（对接 unidentified 表与 ledger）。
- L5. artwork candidates/select（TMDB 海报，复用 poster_fetch）。

## 阶段 4 —— Playback 视图 + 图片（✅ 完成）

- P1. `up-next` / `favorites` / `history` / `activity` / `stats/watch` / `devices`：接 `playback_progress` 表 + ledger。
- P2. 图片端点与 Jellyfin Images 桥接（直连播放已有 9 端点，保持）。

## 阶段 5 —— Notify client（✅ 完成）

- N1. notify config 已有；补事件点发送：subscribe completed / download-transfer failed / check-in failed → POST `jianxcao/notify`，失败仅记日志不阻断。

## 阶段 6 —— 刷流功能（✅ 完成）

- R1. `ratio-boost` / `pause` / `protection` / `boost-stats`：按上游现有方式实现（boost budget/hold days 存 site 行，job 扫描 qB 上传量触发），后期可改。

## 阶段 7 —— 收尾（✅ 完成）

- F1. 剩余 stub 端点逐个真实化（target-prefs、automation-readiness、collections 等有数据来源的）。
- F2. `cargo test --workspace` 全绿 + `pnpm build` 通过。
- F3. 浏览器 E2E 回归：搜索→订阅→下载→入库→媒体库显示全链路（真实站点或 mock）。

## 阶段 8 —— 媒体库实体 + 多文件夹（✅ 完成）

- L6. `libraries` 实体表（kind/name/is_default/sort_order）+ `library_roots` 挂 `library_id`/`sort_order`；既有根回填进默认库。
- L7. `/libraries` CRUD：POST 创建（多根，首根为主根）、PATCH 改名/换根（diff 保留根 id）、DELETE（每类型最后一个库受保护）、PUT `{id}/default`、PUT `order` 全量重排。
- L8. 浏览/维护端点按库收窄：items/item-index/facets 只统计本库根下的 ledger 行（同类型多库互不串）；scan/metadata-refresh 遍历该库全部根；automation-readiness 逐库检查；playback item 库归属按路径命中。
- L9. 前端 `lib/api/libraries.ts` 解除 create/update/delete/setDefault/reorder 五个 stub，接新端点。


## 阶段 9 —— 上游缺口补齐（✅ 完成，分支 scrape-settings 合并 90c8c51）

- 0. 刮削与整理设置：/settings/scrape 配置域（语言/分级/选图/门卡/档位/四段命名/目录写入）+ 全部消费者接入（命名合成、TMDB 语言、选图策略、mirror 开关）+ 前端四 tab 分区
- 1. 播放活动后端：会话/播放记录/统计/设备/进度/up-next/favorites 真实化 + jellyfin 会话喂入 + 超时回收
- 2. 条目详情元数据：NFO 侧车解析 + fanart 背景 + TMDB 详情写富 NFO
- 3. 分集剧照 + 音轨字幕轨：ffprobe 抽取 + file_meta 缓存 + TMDB 分集剧照
- 4. 回收站 + 重复文件：延迟删除/恢复/清理 + 同单元重复分组删除
- 5. 「其他」video 库：不识别扫描、文件名即标题、抓帧封面
- 6. 每库可见性/成员权限：access_mode/admin_visible/member_ids + 浏览收窄
- 7. STRM 播放（302/Range 代理）、洗版替换进回收站、管理页回收站/重复 tab、活动页不可见库会话折叠
- 8. 删除 Photos 模块（前后端坑位全清：photo-wall/photo-lightbox 移除，墙内核保留为 wall.tsx）；新增：章节场景图（ffprobe+ffmpeg，详情条/播放器章节）、洗版保留双版本（keep_old_versions）、演职员人物页（NFO tmdbid + /media/person 复用）、墙筛选真实化 + 筛空放宽建议（relax-filter）
