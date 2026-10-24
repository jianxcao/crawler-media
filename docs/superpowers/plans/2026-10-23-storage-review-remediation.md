# 存储重构剩余优化与验收方案

状态：2026-10-24，本轮起点 `3ba25fb`，本文与补修同一提交。R01–R07 具体反例及 R08 的主要冷 IO 路径已补修，证据见[再次复核](../../audits/2026-10-24-storage-fix-followup.md)。本页只保留剩余优化和验收；已执行步骤从当前计划剔除，旧全文可用 `git show 3ba25fb:docs/superpowers/plans/2026-10-23-storage-review-remediation.md` 查看。

保留五文件、显式归属、事实/摘要同事务、冷详情与持久意图。无旧数据导入、兼容视图、双读双写、旧字段别名或升级 shim；静态图片 public，Jellyfin 视频按协议鉴权。以下是后续方案，不能当作已经实施或已测。

## 1. 根操作与剩余路径 IO 的有限调度（R08 剩余）

create/update/insert-root 目前可同步 drain 全队列；启动恢复虽每批释放 Store，仍同步循环。根配置 canonicalize 和部分 Watch 归属检查等仍需逐入口拆分。

- 根路径在现有 blocking 预算池准备 owned canonical 身份及原 config/root 快照；锁内事务重验并登记持久工作。沿用现有归属与冻结屏障，不加通用队列。
- API 返回持久 operation 状态；后台每次最多一个 100 文件窗口，批间释放 Store、检查取消/停机并让出调度。源/目标冻结时保留 pending，不推进错误 cursor。
- Watch 只在锁内读取登记键和配置；真实路径解析与图片存在性检查在锁外，提交继续校验原 scope。
- 验收：门控实际 canonicalize/NFO/rename 磁盘阶段；并行 Store 读取、Tokio 心跳与停机均限时完成。多 pending root/rename、同 Media 10k 文件/多 User 下记录最大 Store 持锁时间、事务 p95、busy 和 WAL；无锁环证据时报告阻塞/饥饿。

## 2. Unidentified 选择工作量与根迁移守恒

现实现每事务最多移动 100 行，251 行重启回归通过；选择 SQL 尚会归一化 path 并逐候选求最深根，移动行数有界不代表总扫描工作量有界。

- 为 Unidentified 固定登记路径身份并建立索引，使用范围 seek 和持久 keyset/phase；配置变化时重验当前最深根与两侧生命周期，最终复核 cursor 前新写入。
- 用 5k/50k/100k 无关记录测 EQP、VM steps、读取候选数和事务 p95，确保后续批次不反复全读历史。
- 扩展至少 301 文件、同 Media 多目录/season/work/episode NFO 的每批重开测试，比较输入来源、locale/provenance/export digest 与归属守恒。现有跨目录及 alias 回归继续保留。

## 3. 查询和连接策略按测量决策

- 固定 ACL、locale、跨库去重、稳定排序和精确 total，测 5k/20k/50k/100k 大命中 genre 的 EQP/VM/p50/p95/锁时间；再决定有序成员索引或局部读模型。keyset 不会消除精确 count，不能改近似 total。
- person/search 没有新证据要求引入 FTS。cache NORMAL、mmap、cache_size、temp_store、WITHOUT ROWID 逐项 A/B，记录耐久、内存和 WAL 取舍；Jobs FULL 及其连接级回归已完成，不重复列待办。
- A8 竞争写者行为 RED、A12 NFO export 区分性失败 RED 单独补验收；缺少反例不能直接判当前生产行为错误。

## 4. 现场与完整规模验收

真实浏览器/Jellyfin 设备、NAS、完整停启/重启/清 cache 与 100k Media / 1M 文件 / 1M Playback 组合测试仍为 **Not run**。使用隔离实例，记录源码版本、数据形状、原始日志及失败原因，不操作真实用户实例。历史 50k 或无关库 1M 测量不能代替此门。

每次新修复按公开行为 RED→GREEN，遵守 AGENTS 的文件/函数上限；受影响 crate 测试通过后跑 workspace。生产 DDL 为唯一结构来源；完成项从本页移除，证据留固定版本审计，不重新生成多份冲突步骤文档。
