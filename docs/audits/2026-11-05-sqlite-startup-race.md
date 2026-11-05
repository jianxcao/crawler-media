# SQLite 首次并发初始化崩溃

日期：2026-11-05。范围：Apple Silicon、rusqlite 0.37.0、libsqlite3-sys
0.35.0 bundled SQLite 3.50.2；不涉及数据库格式、SQL、读池容量或删除语义。

## 实际失败与根因

前缀过滤和剧集分页修复后的稳定代码树执行 release workspace，退出101。
595个结果组汇总3942 passed、0个计入汇总的断言失败、25 ignored（含子进程，
非去重）；`api --test executor_not_blocked` 在五项测试完成前 SIGSEGV，故该轮
不能标绿。单独重跑五项通过，没有消除原失败。

macOS崩溃报告与当时可执行文件的UUID一致。故障线程从 `Store::open` 经
rusqlite的线程模式检查进入 `sqlite3_mutex_alloc` / `sqlite3_initialize`，
调用空的 `xMutexInit`；另一测试线程同时处于SQLite初始化。旧ARM指令在读取
已发布的 `xMutexAlloc` 后、读取并调用 `xMutexInit` 前缺少读屏障。

[SQLite官方修复](https://github.com/sqlite/sqlite/commit/a6f89b9d0b5be75f5cdd2d6c8e6bdc6c37d092ad)
于2026-10-20给该分支增加配对的 `sqlite3MemoryBarrier()`，对应上述缺口。
截至核查，未确认包含它的已发布SQLite/rusqlite版本，故没有以版本升级代替
修复，也没有降低测试并发或串行化普通查询。

## 修复与回归

根Cargo patch引用受版本控制的完整发布包，仅回补官方六行C代码。原始archive
checksum、上游身份、唯一差异及移除条件见 [vendor说明](../../vendor/README.md)。
共享原生引擎因此同时覆盖Store、Jobs、Media及原始rusqlite连接；无需让各crate
分别维护容易遗漏的初始化guard。系统SQLite和可选SQLCipher不在本轮验证范围。

新增公开入口回归每轮启动16个全新进程，每进程同步8个首次打开线程，覆盖Store、
Queue、响应缓存和原始连接的实际写入、读取及路径隔离；交替使用空目录和已有库，
包含只读preflight。每个子进程有30秒生命周期上限，超时仅终止并回收自有子进程，
保留输出后失败，不重试。

该回归在补丁前也通过，不能伪称它稳定复现了弱内存序竞态。有效RED是原始实际
SIGSEGV，结合官方同点修复和旧二进制指令证据。补丁后定向启动及原失败目标7项
通过；新ARM二进制明确在读取 `xMutexAlloc` 后、读取/调用 `xMutexInit` 前发出
`dmb ish`。这提供运行结果之外的修复落地证据，不等于数学证明不存在所有竞态。

最终稳定树release workspace完成597个结果组，3763 passed、186 failed、25 ignored
（含子进程、非去重），退出101；该次缺少回环端口权限，20个目标出现182处直接
PermissionDenied及连带断言失败，未再发生原生崩溃。保留原轮退出101，在相同
代码hash下授权补跑且仅补跑这20个目标：六个crate命令全部退出0，汇总1096 passed、
0 failed、2 ignored。源代码hash前后一致，失败目标集合与补跑集合逐项对应；此为
全量加环境失败目标补跑的完整验证，不将单次workspace退出改写为0。release
workspace all-target Clippy及格式检查退出0；限定独立静态复核无剩余可操作问题。
Linux/Windows运行时验证为Not run。原失败日志、补跑及不同二进制身份分别保留，
不覆盖历史结果。
