# 百万文件夹具的有界 COMMIT 观察

本轮在 `2f5cd923f95f` 的独立 APFS 副本中没有复现历史 2.735 秒 COMMIT。临时诊断与无插桩控制均实际退出 0；这关闭本轮观察，不关闭历史尾延迟风险，也不替代此前完整容量终态。

## 边界与实际结果

预先限定：最多十分钟到达 Users，随后最多五分钟或十万次 Users 推进；捕获首个至少 100ms 的 COMMIT PROFILE 区间后再取八个正常控制。两轮共用十六分钟上限，按顺序独占运行，保留至少 30GiB 空间。不改变 SQLite 策略，不安装会替换默认 autocheckpoint 的 wal_hook。

两份副本前置均核验 100,000 Media、1,000,000 文件、1,000,000 Library unit、1,000,000 播放单元、10 用户及外键错误 0。有效设置均为 WAL、`synchronous=2`、`wal_autocheckpoint=1000`。

| 观察 | 实际停止 | 推进循环耗时 |
|---|---|---:|
| 临时诊断 | 224,978 步；其中 100,000 次 Users→Users；users_limit；applying | 72.049298500 秒 |
| 无插桩控制 | 同一步数、转换及状态 | 71.849938542 秒 |

诊断覆盖计数缺失为 0；十万次 Users COMMIT PROFILE 区间中没有达到 100ms 的样本。只保留最后 16 个正常样本的详细 VFS 事件：704 条，包含 400 次写、16 次同步、288 次共享内存锁，返回值均为 0，数量及时间关联通过。未保留全部推进的 VFS 明细，不能由此排除其他事务或 PROFILE 后的长 checkpoint。固定顺序和运行环境未控制，两轮差值不精确代表插桩成本。

本次 VFS 转发真实 SQLite 操作，记录写、同步、普通锁、共享内存锁及 checkpoint 标记；保留推进包络内 PROFILE 前、重叠、之后的事件。当前 vendored SQLite 在 PROFILE 回调后执行 WAL callbacks，因此不能仅凭 COMMIT 名称把历史 PROFILE 内的 2.735 秒归因于默认 checkpoint。原历史峰值副本已删除，具体同步/调度/锁来源仍未确认，没有据此调整持久性、checkpoint 或锁策略。

## 验证、隔离与失败

临时工具的实际验证为 VFS 真实 SQLite probe 1 项、策略 2 项、Python 报告校验 2 项及 harness 8 项通过。之前 partial move、查询缺字段及 unused import 的编译失败日志保留，修正后重新执行通过；临时 harness 仍有旧的未使用诊断类型警告，不声称零警告构建。

首次启动因空闲 40.66GiB 低于“30GiB保留 + 完整夹具逻辑体积8.05GiB + 3GiB”被拒绝，未构建或推进。随后无损压缩两份已结束的旧 backend 日志，流式解压 SHA-256 一致后删除原文本，回收约 1.15GiB；还原信息见旧日志目录 `compressed-evidence-20261106.json`。没有删除用户媒体或数据库夹具。

正式观察以 `--locked --offline` 构建；相关产品依赖、workspace 配置、vendor、临时 harness/VFS/driver/validator 及 Cargo.lock 绑定摘要，构建前后和结束时复核。两个副本顺序创建，原夹具文件 inode/大小/mtime 清单前后一致；这是元数据核验，不冒称完整内容哈希。副本已删除，没有修改原 app、STRM 或 pan。

原始回执：`/tmp/crawler-commit-tail-receipt-q6reye2t/receipt.json`，首次空间拒绝：`/tmp/crawler-commit-tail-receipt-1zyyvt7v/receipt.json`。本轮为部分诊断，`acceptance_eligible=false`；没有重新运行完整 450 万步容量终态、GUI、491 来源或内存测试。历史完整容量计数/外键通过与历史 2.735 秒风险继续分别保留。
