# 外部来源响应缓存：统一文件、按来源拆表

状态：**设计修订，未实施**。本设计与 [Media / Library 存储重设计](2026-10-18-library-storage-redesign.md) 配套，以后者为整体架构准则。用户允许全部舍弃旧数据库数据；不需要导入、兼容层、双读双写或旧游标转换。

## 1. 职责边界

`cache.db` 只保存外部接口响应，减少重复网络请求；不充当本地 Media 身份、已采用元数据、Library 浏览摘要或观看事实的唯一来源。

- 六张固定来源表：`tmdb_cache`、`douban_cache`、`tvdb_cache`、`bangumi_cache`、`anilist_cache`、`fanart_cache`。
- **不创建 `_read_revisions`、`_browse_changes` 或浏览消费游标，不安装浏览失效触发器。** 旧设计中“共享一套journal”的要求废止。
- 识别、NFO导入、明确元数据刷新等业务动作，可以经响应缓存取值并将标准化元数据采用到 library.db；仅仅更新/删除响应缓存不能自动修改本地已采用元数据。
- 清空/重建cache.db后，现有Library列表、筛选、已采用图片地址与Playback仍可离线读取；下一次外部请求可能重新访问网络。
- `cache.db` 不保存图片/视频字节；本地artwork由文件服务提供，远端图片按URL读取。

## 2. schema 与共享实现

六表相同，以TMDB为例：

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

- 固定 `CacheSource` 枚举映射表名；source参数先解析，不能拼接用户提供的SQL标识符。
- media提供共享响应缓存schema/SQL，Store和六客户端复用；不复制六套实现、不新增crate。
- cache_key唯一索引点查；UPSERT保留entry_id；不同来源同键隔离。
- body保留原始响应，端点校验后才替换。语言、请求参数、fanart凭证摘要遵循既有键隔离。
- 默认TTL **604800秒（7天）**，时间Unix秒，NULL不失效，expires_at<=now已过期；刷新失败可回退最后有效stale响应。存储错误不是cache miss。
- 详情前缀只匹配exact和`prefix?参数`，不含其他数字ID或子资源；用BINARY范围，不用未转义LIKE。
- 六表合并列表顺序为fetched_at DESC、固定来源顺序TMDB/Douban/TVDB/Bangumi/AniList/Fanart、entry_id ASC。保留HTTP/CLI的来源名和DTO，不把SQL表名当API参数。
- 独立schema从1开始；幂等初始化、未知/较新布局拒绝打开。连接继续WAL、5000ms busy_timeout、foreign_keys=ON；同文件仍共享写锁。

## 3. 管理与维护

- Store缓存连接及六客户端全部指向cache.db，不创建catalog.db/fanart.db。新布局维护目标app/cache/library/subscribe/jobs。
- 缓存管理保留列表/来源统计/定向删除；聚合页面不把fanart原始图片候选冒充Media。来源读取失败向上抛，不能省略失败表仍返回成功。
- 分类保留仍默认关闭、dry_run=true、90天窗口，详情与未知key保护不变，不额外增加到期即删除。
- transient全局每批examined<=500、单次最多20批或2秒（批间检查）。每表最多预取limit+1，6表总预取不超过6×501；检查和删除预算不能按来源倍增。
- 游标为fetched_at对应毫秒时间与`<source>:<entry_id>`，无旧rowid转换。预览零写、停用/取消阻止后续批。
- 受控清空走连接内删除或停写关闭全部连接后重建，不能直接unlink活跃数据库/WAL。SQLite空间复用不等于即时缩小文件；checkpoint/VACUUM继续按已有维护控制。

## 4. 验收与切换

1. 六来源各自hit/miss/expiry/NULL/stale/坏响应不覆盖、SQL错误传播；跨来源同键/语言隔离、未知source注入、prefix `%`/`_`/Unicode边界。
2. 同来源无关行1k→50k，点查/定向prefix输出相同，VmStep<=基线2倍、FullscanStep=0；跨来源增长不使目标查询全扫描。记录写成本/空间，不声称拆表解决并发写。
3. 跨六来源100k候选，examined<=500、详情保护、preview零写、首批停用后不执行第二批。
4. 实际临时实例：fake六客户端→API管理→采用本地元数据→浏览/播放→受控清缓存→重启，已采用元数据与本地列表/观看状态不变，正常启动不生成旧文件，无浏览重建任务。
5. 每触及crate独立测试，整批workspace一次；涉及web则typecheck/test/build及实际页面。NAS/真实来源未运行写Not run。

全部旧DB可在实施最后停服后一次性重建，只删除明确实例SQLite文件与WAL/SHM，不动视频、字幕、NFO、图片、下载目录、Indexer YAML或编译缓存；正常启动不自动清库。旧数据可丢，不代表可以交付无法扫描、Transfer、Scrape、浏览或播放的新schema。

原 [缓存拆表实施计划](../plans/2026-10-18-source-separated-cache.md) 已被整体重设计取代，**不得继续按旧步骤实施**。整体设计审阅后再生成配套的新实施计划。
