use crate::FilterId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
    pub id: FilterId,
    pub name: String,
    pub atoms: Vec<FilterAtom>,
    pub keep_old_versions: bool,
}

impl Filter {
    pub fn new(id: FilterId, name: impl Into<String>, atoms: Vec<FilterAtom>) -> Self {
        Self {
            id,
            name: name.into(),
            atoms,
            keep_old_versions: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterAtom {
    pub priority: i32,
    pub rule: AtomRule,
    /// exclude 原子命中即排除（黑名单语义：平台/制作组/HDR 黑名单）。
    pub exclude: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AtomRule {
    Resolution(String),
    Source(String),
    Free,
    Hr,
    Codec(String),
    TitleMatch(String),
    /// HDR 格式（Release.hdr，如 `HDR10` / `DV`；区别于 `Hr` 的考核标志）。
    Hdr(String),
    /// 体积区间（MiB，按单集均摊比较：整季包用总体积 ÷ 集数）。
    Size {
        min_mb: Option<u64>,
        max_mb: Option<u64>,
    },
    /// 做种数下限。
    MinSeeders(u32),
    /// 要求的字幕语言（规范化 BCP 47，如 `zh` / `zh-Hans` / `en`）。
    SubtitleLanguage(String),
    /// 要求的音轨语言（规范化 BCP 47，如 `cmn` / `yue` / `en`）。
    AudioLanguage(String),
    /// 站点白名单（SiteId）。
    Site(String),
    /// 洗版目标档位（如 `2160p Remux`）：不参与 admit 打分，只被 wash-target 读取。
    WashTarget(String),
    /// 洗版比较维度顺序（逗号分隔，如 `resolution,source`）：配置后替换判定
    /// 从「分数更高」改为按维度逐项比较新老版本。
    UpgradeLadder(String),
}
