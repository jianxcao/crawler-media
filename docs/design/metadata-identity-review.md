# 元数据身份复核

文件与 Media 的当前归属来自 SQLite。文件名、标题和年份可以缩小候选范围，但同名同年的 TMDB 条目仍可能有不同 ID；搜索顺序或热度不能作为唯一身份的证据。自动识别继续要求唯一可信身份，歧义保留为待确认状态（pending），不等于探测或元数据抓取任务失败（failed）。

## 只读候选预览

`GET /api/v1/libraries/{id}/items/{item_id}/reidentification-preview` 先捕获当前 Media、Media revision 和文件的 FileVersion，并释放 Store 读快照。Catalog 搜索及文件海报观察在后续 blocking 工作中完成，不持 reader lease 或 writer 锁，也不修改 Media、文件归属、FileVersion 或 sidecar。

没有任何外部 ID 的 Movie/TV 才搜索 TMDB；已有 TMDB、Douban、TVDB、Bangumi 或 AniList 身份的条目只回显已保存身份，预览不会无端重新解析并替换它。搜索使用文件名解析出的名称，缺失时使用当前 Media 名称；TV 请求同时传递已知年份。候选必须与当前条目类型一致，已知年份时要求候选年份也相同，按 TMDB ID 去重，最多返回 20 个。

| 预览结果 | 含义 |
| --- | --- |
| `ambiguous` | 多个身份符合条件，展示各自 TMDB ID，等待选择 |
| `no_match` | 搜索完成，但没有符合类型和已知年份的候选 |
| `tmdb_unreachable` | 搜索上游出错，可以稍后重试；不同于没有匹配 |
| 单候选、`code: null` | 提供待确认候选，不自动绑定 |
| `source: current` | 保留已保存的身份，不发起重新搜索 |

这些结果只解释预览。关闭窗口不会写任何身份；零候选或网络失败也不会清除当前 Media 的信息。

## 手动选择与 CAS

候选按钮显示名称、年份与 TMDB ID，点击后沿用详情确认面板。用户确认后，客户端向既有 `POST /api/v1/reidentify` 提交 `title_ref: "tmdb:<movie|tv>:<id>"`、预览的 `media_item_id/source_revision` 以及文件的 `file_id/revision/source_version`。

服务端解析选定 ID，并通过既有归属重绑 CAS 校验 Media revision 和每个文件的 FileVersion。预览后发生源变化、归属变化或其他并发编辑时，提交会拒绝过期观察，客户端须重新预览；服务端不代填新版本。目标外部身份已属于另一 Media 时继续遵守现有身份冲突规则，不绕过唯一性约束合并。

成功后文件归属与目标 TMDB 身份持久化，后续预览回显所选 ID；扫描也继续保留已经确认的外部身份。元数据/sidecar 刷新使用既有流程，其失败不会撤销已经完成的身份重绑。
