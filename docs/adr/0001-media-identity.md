# 内部 Media id，外部目录 id 作为别名

**Media** 记录使用系统分配的 id。TMDB、Douban、TVDB、Bangumi、AniList 的 id 是该记录上可空的别名，而非主键。Subscribe、媒体库路径、刮削产物与缓存都指向内部 id。

v1 需要五种 **Metadata source**，包括没有 TMDB id 的作品。把 TMDB id 当作主键会禁止这类 Subscribe。以 `(source, id)` 作为主键则会让路径与 subscribe 行在识别选择了不同来源时漂移。内部 id 只多一张表，外加两个别名后来被证明是同一作品时的合并路径。

**备选方案**：仅用 TMDB 主键；复合 `(source, id)`。
