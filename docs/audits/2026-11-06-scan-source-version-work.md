# 2026-11-06 扫描源版本同值更新

## 问题和修改

普通重扫会为新 operation 写入自己的断点。`record_scan_seen_stamps` 同时把版本写入 `library_files`，即使已存的纳秒时间、大小和内容摘要与本次赋值完全相同，也执行该行更新。

修改只在这三个字段至少一个不同时更新 durable 源版本。比较使用实际写入的 `mtime_ms * 1000000` 和空值安全的 `IS NOT`，因此仍会把带亚毫秒部分的旧时间推进到原先规定的毫秒精度。operation 断点仍在同一事务中写入，路径别名快照、执行顺序和回滚边界保持原语义。

## TDD 和工作量

新增公开 Store 集成测试 `scan_seen_work`。100 个已登记文件先写初始版本，再分别用新 operation 重扫同值版本、推进变化版本。原代码的两条路径均消耗 8,608 个 SQL VM steps，checkpoint 和 durable 读回正确，只有“同值路径应少做工作”的断言失败。

修改后，同值路径为 5,508 steps，比原来少 3,100（约 36.0%）；变化路径为 9,308，比原来多 700（约 8.1%）。这是所测操作的 SQLite VM 工作量，不能换算为物理写入量、锁持有时间或 HTTP 延迟改善。真实变化路径承担新增字段比较成本，此限制与收益一并保留。

三个新测试还检查：mtime、size、digest 分别变化，NULL 与摘要的双向转换，亚毫秒时间精确处理，重开数据库后的版本和断点，以及第 100 个 stamp 输入非法大小时整批断点和源版本回滚。断言通过公开 Store 读写接口完成；初稿的私有 SQLite trigger 失败注入在审查后删除。已有别名消失后的路径身份测试及扫描源版本测试共 12 项通过。

定向命令在两把 Cargo 槽锁内执行，设置 `TMPDIR=/tmp`，使用现存 `/tmp/cm-marker-build.a22APb` 缓存：

```sh
cargo test --offline -j8 -p store \
  --test scan_seen_work --test scan_recorded_keys --test scan_operations \
  -- --test-threads=8 --nocapture
```

定向结果为 3 + 1 + 11 项通过；修正公开失败注入后，新文件 3 项再次通过。独立静态审查无剩余发现，记录在 `/tmp/cm-scan-seen-test-review.md`；RED 二进制为 `/tmp/cm-scan-seen-work-red-f67b4229`。随后 Store 全套实际退出 0：208 个结果组汇总 1,038 passed、0 failed、12 ignored（含嵌套结果，非去重数）。`cargo clippy --offline -j8 -p store --all-targets -- -D warnings`、`cargo fmt --all -- --check` 及 `git diff --check` 均退出 0。全套原始日志和汇总分别为 `/tmp/cm-scan-seen-store-full.log` 与 `/tmp/cm-scan-seen-store-full-summary.json`。

## 与剩余验收的关系

此修改关闭的是已证实的同值更新工作浪费。此前分流诊断中的 played writer 等待与 scan 区间重叠，没有定位到本函数，不能据此宣布该慢尾或 progress 相对门已修复。原正式相对失败、首次上游推荐的完整响应耗时及 FULL 同步尾延迟仍按原记录保留。

源码与现有数据复核还排除了两项猜测：两版 TMDB 都持久缓存 typed 404 七天，分流轮的 340 个 pending 窗口对应不同 key；正常已绑定的 self/Jellyfin progress 和 played 均只取得一次 Library writer，没有发现新增重复 summary reduction。实际失败的 progress 用例是 self API，明确携带 duration，不会进入缺少 duration 的 probe 入队分支。这些结论不证明全部请求无其他问题，也不把外网 HTTP 区间归因于某个网络组件。只读报告分别为 `/tmp/cm-similar-source-comparison.md` 和 `/tmp/cm-progress-source-comparison.md`。
