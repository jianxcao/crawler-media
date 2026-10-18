# 外部来源缓存统一文件、按来源拆表设计

状态：供审阅，尚未实施。用户已明确：**项目未上线，现有数据库数据可以全部舍弃，不要求兼容或迁移旧数据。** 本次只设计缓存拆表，不重做其他业务模型，不实施 DB-07。

## 1. 范围与切换原则

- 唯一运行时缓存文件是 `{data_dir}/cache.db`；TMDB、Douban、TVDB、Bangumi、AniList、fanart.tv 各一表。
- 不写旧缓存导入、历史格式兼容、完成标记、双读双写、兼容视图或自动升级旧 catalog/fanart 文件。
- 实施完成后停服务，确认真实数据目录及自定义 app 路径，**一次性重建开发数据库**。可删除的仅是实例的 app/catalog/library/subscribe/jobs/fanart/cache SQLite 文件及其 WAL/SHM；不删除视频、NFO、图片、下载目录、Indexer YAML 或 Cargo 编译缓存。
- 删除是实施时明确的人工步骤，不放进正常启动逻辑；本次编写计划不执行删除。
- 用户名、设置、Site/Downloader 凭证、Subscribe、观看进度、历史、缓存等旧 DB 数据都不保证保留；之后由新程序初始化并重新配置/扫描。
- 仍保留各来源客户端的既有行为与 `/api/v1/catalog/cache` 路由/DTO，避免无关的前端/协议改造。

## 2. 物理 schema

固定表名：`tmdb_cache`、`douban_cache`、`tvdb_cache`、`bangumi_cache`、`anilist_cache`、`fanart_cache`。六表结构相同，以 TMDB 为例：

```sql
CREATE TABLE tmdb_cache (
    entry_id INTEGER PRIMARY KEY AUTOINCREMENT,
    cache_key TEXT NOT NULL COLLATE BINARY UNIQUE,
    body TEXT NOT NULL,
    fetched_at INTEGER NOT NULL,
    expires_at INTEGER
);
CREATE INDEX tmdb_cache_recent_idx ON tmdb_cache(fetched_at DESC, entry_id ASC);
CREATE INDEX tmdb_cache_retention_idx ON tmdb_cache(fetched_at ASC, entry_id ASC);
```

- `cache_key` 唯一索引负责点查和详情范围查询；`entry_id` 为本来源稳定游标/时间并列消歧键，不是 Media 身份。
- UPSERT 更新原行，不替换 entry_id。各来源同键互不影响。
- 时间为 Unix 秒；默认 TTL **604800 秒（7 天）**，`expires_at=NULL` 不过期，`expires_at<=now` 已过期。
- body 为原始响应文本，各端点仍在覆盖缓存前解析/校验。保留语言/参数/fanart 凭证摘要的原 cache_key 规则。
- 缓存过期不等于允许删除：网络失败可使用最后一份可解析的 stale 响应。
- 六表共享代码中的 `CacheSource` 固定枚举，表名只能从枚举生成；未知 source 返回参数错误，绝不拼入用户表名。
- 在既有依赖方向中由 `media::response_cache` 提供共享 schema/SQL，Store 与客户端复用；不新增 crate 或外部依赖，不复制六套缓存实现。
- cache.db schema 从 **1** 开始，初始化幂等，较新版本拒绝打开。其他 DB 的版本与历史迁移不属于本次重整范围，不因文件改名随意重置。
- 连接显式设置 WAL、5000ms busy_timeout、foreign_keys=ON。六张表仍在同一文件，不能宣称消除了 SQLite 写锁竞争。

## 3. 变更通知与重启

- 为保留已有浏览缓存正确性，cache.db 仍有**一份** `_read_revisions` 与**一份**有界 `_browse_changes`，不是每来源各一套。
- 六表 INSERT/UPDATE/DELETE 都推进修订并写逻辑 `catalog_cache` 事件；实体键仍为 `<source>:<cache_key>`。逐行事件阈值 **1024**，超出合并为 browse_all。
- `ReadRevisions.catalog` / `BrowseChangeSource::Catalog` 暂保留逻辑名称，实际连接 cache.db，避免顺带重写其他 DB 的 DTO。
- 整套开发 DB 重建后游标自然从头开始。另需支持以后单独丢失 cache.db：启动发现新建缓存文件时，在启动客户端/后台服务之前重置 library.db 的逻辑 catalog 游标并标脏已有浏览投影。不需要旧数据导入或迁移完成标记。
- 已有 cache.db 的正常重启不重置游标；失败不能伪装成成功，也不能漏掉一张来源表却返回更小的成功列表。

## 4. 运行时入口与维护

- Store 的 catalog 连接换成 cache；main 的五个 Metadata source 和 scrape 的 fanart 客户端全部传同一个 cache.db。
- 保留现有 Store 的 `put/get/list/delete_catalog_cache`、`catalog_details_batch`、`catalog_details_for_prefixes` 业务接口，内部切换到固定来源表；HTTP source 仍用 tmdb/douban 等来源名，不用 SQL 表名。
- 详情前缀只匹配 exact 与 `prefix?参数`，不匹配其他数字 ID 或 `/images` 子路径；使用 BINARY 范围查找，`%`/`_` 不是通配符。
- 合并列表固定排序：`fetched_at DESC`、来源顺序 TMDB/Douban/TVDB/Bangumi/AniList/Fanart、`entry_id ASC`。同来源详情保持原来“同时间先插入者优先”的规则。
- 维护目标变为 app/cache/library/subscribe/jobs，不再维护独立 catalog/fanart 文件。
- 分类保留仍默认关闭、dry_run=true、90 天窗口；不增加自动到期删除。transient URL 白名单、详情/未知 key 保护不变。
- 六表清理按 `(fetched_at ASC, source固定顺序 ASC, entry_id ASC)` 合并 keyset 窗口；全局每批最多 **500 个 examined**，单次维护最多 **20 批或 2 秒**（批间检查）。每表最多预取 limit+1，总预取最多 6×501，不能把预取量说成只有 500。
- DataRetentionCursor.at_ms 继续为毫秒，id 使用 `<source>:<entry_id>`，不再用旧共享表裸 rowid。此次整体数据重建，不设计旧游标转换。
- 静态图片公开路由、Job 清理开关和其他业务权限不变。

## 5. 验收与非目标

五类必测：来源/键隔离；TTL/解析失败/stale；缓存文件丢失后重建与失效游标；有界清理及停用；数据量下的真实 SQL 工作量。

- 同来源无关行从 **1k→50k** 时，定向点查/详情前缀输出一致，VM steps 不超过基线 **2 倍**、FullscanStep=0；其他来源增加 50k 行不增加目标表点查 VM steps。
- 跨六来源 **100k** 清理数据，一批 examined<=500，详情/未知 key 不删；首批提交后停用，不执行第二批删除。记录预取/检查/删除三个数量。
- 烟测实际运行临时 Store、fake HTTP 六客户端、缓存管理 API，确认六表命中、重启可用、fanart 与 Store 共享文件、正常启动不生成 catalog.db/fanart.db；错误向上传播。
- 读写压力测试验证无来源串扰、死锁和静默丢写；不承诺同文件多写者并发提交。
- 每个触及 crate 独立测试，稳定树 workspace 一次；前端文案涉及则 typecheck/test/build。NAS/真实大数据设备验收按用户决定延后并写 Not run。

交付更新 docs/database、ADR-0008、部署重建说明和实测记录；不做六 DB 整体重设计、DB-07、生产数据迁移、图片二进制缓存或任意路径删除工具。
