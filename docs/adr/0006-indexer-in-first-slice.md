# 第一个垂直切片包含真实 Indexer 引擎

第一个可运行版本要与真实 PT 站点通信。它不是 fixture 种子列表。引擎覆盖：NexusPHP HTML 搜索（框架默认 + 每站 YAML）、RSS 拉取、M-Team 的 API 作为代码级例外、跨启用站点并发搜索、以及无需重新构建即可替换内置 YAML 的用户 overlay 目录。

假 Indexer 只能证明 Filter → qB → Transfer。产品主张是“搜索站点、下载种子、归档到媒体库”；在 enclosure 字节来自真实 tracker 之前，这条路径未被证明。切片一就交付完整引擎比单个 NexusPHP YAML 成本更高，但避免了第二个站点一出现就得重写 Indexer crate。

**备选方案**：fixture Indexer；仅一个 NexusPHP 站点；仅 M-Team 首版。
