# Subscribe 搜索精度：现行规则与剩余核验

**2026-10-20 整理，源码基线 `bd6c3c8`。** 原任务已有实现（`63db4bd`及后续合入），不是全部待开发。本页保留搜索规则、原需求歧义和未关闭验收；删除重复的任务3步骤、预计编译失败／提交脚本，不强制新存储计划执行独立搜索工作。完整原稿可由 `git show bd6c3c8:docs/superpowers/plans/2026-10-16-subscribe-search-precision.md` 追溯。

## 1. 已有实现与规则

源码：`crates/api/src/worker/search.rs` 的 `search_keywords_for`、`search_torrents_for_subscribe`、`page_is_full_and_irrelevant`；`crates/release/src/boundary.rs` 的 `plausible_episode` 及解析调用。现有回归入口 `crates/api/tests/subscribe_search_keywords.rs`、search模块测试、`crates/release/tests/parse.rs`。本次只读源码／文档，不运行测试，不把文件存在说成当前验收通过。

- 先放原标题、主标题／有用短标题及年份组合，再按详情顺序最多追加3个合格且归一化不同的别名；合格后再截断。归一化忽略大小写和分隔符，不按TMDB／IMDb ID搜索Site。
- 原始Metadata别名与“可发给Site的搜索词”不同：保留完整别名不等于每个别名都产生请求。当前门槛为ASCII词≥2或非ASCII非空白字符≥2；`Dream`这样的单词不单独作为别名请求，`Chinese Dream`、`醉梦`、`Pan's Labyrinth`可以。主标题／原标题不受该别名门槛删除。
- 有上界的 `Coverage::Tv { episode_to: Some(_), .. }` 追加一条 `{短标题或主标题} S{season:02}`，无集号／Season 1变体；movie和开放窗口不追加季关键词。
- 每个关键词先请求第1页；**只有季关键词可请求第2页**：第1页至少30条，且没有任何标题匹配本Media／同季的Torrent。已有本季任意一条就停，即使该集已拥有；只有别剧／别季且满页才继续。没有第3页，下一轮重新从第1页开始，不记录“站点已见最大集”或按缺集推算页数。
- “最多2页”指符合条件的季关键词页码上限，不是整个订阅一轮只允许2次Site请求；主标题／原标题／年份／别名仍各有第1页。原稿两个口径已区分。
- Indexer只做Site适配，关键词策略在API，仍共用Site.rate_limit_per_minute；第2页也是一次受限请求，不sleep／冷却循环绕过限制。
- Release集号只接受1..999；合法20YYMMDD日期包括E／EP前缀不能变集号，拒绝日期不顺便推断S01；S01E01／E05／中文集号／区间仍有效。Coverage::MAX_EPISODES=1000不是允许解析E1000的理由。

## 2. 原稿歧义与尚需核验

1. **单词别名歧义未获产品裁决。** 原稿一方面规定拉丁别名至少2词，另一方面要求单词 `Inception` 作为别名保留，互相矛盾。当前实现按≥2词，因此Inception作为主标题／原标题可搜索，作为独立别名被过滤；这不是“全部原验收已满足”。如需放行特定单词别名，须明确可推广的区分度合同与公开测试，不能特殊判断Inception或为文档清理改业务。
2. **多Site分页是未关闭语义门。** 当前gate查看一次fanout合并的 `outcome.torrents`，不是各Site独立判断；某Site有本季命中可能阻止另一满页无关Site翻页，多个未满页Site合并也可能达到30。原单Site用例不证明多Site期望成立；后续应明确按Site还是合并页的合同并在公开seam复现，不在本次偷偷修改。
3. 别名详情存储在新Media／Metadata聚合模型下的完整性、更新保留多源alias、实际搜索入口／手动run一致性，需要当前链路证据，不能继续执行旧“给app.media加JSON列”的迁移步骤。存储alias丢失等问题按当前存储计划处理，搜索独立歧义不自动成为其任务。
4. 保留原验收场景：满30条别剧→页2、满30条本剧别季→页2、任一本季→只页1、不满30→只页1；电影100条也只页1、开放窗口无季词；无第3页，按enclosure去重，多个订阅同进程仍共用限速。
5. 日期样本：裸20261002、点分2026.10.02、E20261002／EP20261002不得生成集号；`Show.20261002.E01`仍解析E01，S01E03-E04保持范围。关键词样本须覆盖第4个合格别名被截断、大小写／分隔符去重与无关ID词不发请求。
6. 2026-10-16猫站实测“名称有结果、TMDB／IMDb形式ID无结果”仅是当时站点观察，不泛化所有Site能力；本合同仍不新增ID搜索参数。后续授权实施使用fake Indexer／fixture，不访问真实Site／TMDB／qB／Chromium；记录真正的RED／GREEN和未测项，不照抄旧命令中的预计结果。
