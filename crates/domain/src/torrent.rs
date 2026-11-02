use crate::SiteId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Torrent {
    pub site_id: SiteId,
    pub title: String,
    pub enclosure: String,
    pub size_bytes: Option<u64>,
    pub seeders: Option<u32>,
    pub free: bool,
    /// None 表示站点未提供可确认的 H&R 状态。
    pub hr: Option<bool>,
    pub imdb_id: Option<String>,
    /// 站点内种子 id（详情/下载链接提取；RSS 用 guid）。
    pub id: Option<String>,
    pub leechers: Option<u32>,
    pub snatched: Option<u32>,
    /// 发布/上传时间（站点原文或 RFC3339）。
    pub upload_time: Option<String>,
    pub detail_url: Option<String>,
    pub category: Option<String>,
    pub poster_url: Option<String>,
}
