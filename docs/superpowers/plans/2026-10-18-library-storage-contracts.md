# Media / Library 开发合同附录

状态：开发计划的规范性附录，**不是当前已实现API**。与[主计划](2026-10-18-library-storage-redesign.md)、[主设计](../specs/2026-10-18-library-storage-redesign.md)一起阅读。新字段/公开符号改动前用LSP references枚举调用方；不靠猜名字/只改编译报错完成迁移。

## 1. DDL、布局与保留范围

- [library SQL](2026-10-18-library-storage-schema.sql)是library.db完整目标建表/索引草案，包含45张代码表；不是生产迁移，不由运行时从docs读取。
- [partition SQL](2026-10-18-storage-partitions.sql)分别包含APP、SUBSCRIBE、CACHE三个有标记区段，各自在独立空连接执行；不能把三个区段执行在同一个文件。
- 实施Task1将上述DDL按职责落入 `crates/store/src/schema/v1/{media,library,playback,operations,probe,app,subscribe}.sql` 和 `crates/media/src/response_cache/schema.sql`。每个生产schema文件不含外层PRAGMA/BEGIN/COMMIT，由初始化器事务包裹；docs SQL随后从实际DDL生成，避免长期维护两个事实来源。
- 新布局application_id：app=1129139248、library=1129139249、subscribe=1129139250、cache=1129139251；四者user_version=1。jobs继续现有Queue schema/JobKind/change_state/retention_cursors，不改队列格式；这次不对其另设layout编号。
- 仅sqlite_schema无任何用户对象且application_id/user_version均0的文件可初始化。所有已存在文件先READ_ONLY预检，任一旧布局、编号不匹配、未来版本或损坏时拒绝且不改文件/WAL模式。已存在目标布局只校验，不重复执行CREATE/seed修复损坏表。第一次初始化各文件独立事务、无跨库原子承诺；启动中断可以重启补齐尚为空的新文件，所有文件通过后才seed默认配置并启动worker。
- WAL/5000ms busy_timeout/FK ON显式设置；维护独立连接仍短超时/no CREATE，不借初始化修改现有维护策略。Store::open_at继续仅自定义app路径；db_paths返回真实app/cache/library/subscribe路径，Queue目标从现有真实jobs.db取得。
- app保留用户密码/角色/token TTL/Site Browser/凭证/Downloader映射/Filter/Naming模板/settings/search snapshots。删除Media/Library/Collection/浏览revision建表和legacy password/recycle/旧进度数据迁移代码；**只删除数据兼容，不删token头格式或Jellyfin协议兼容**。
- subscribe保留4张策略/事实/待下载表及其全部现行新增列，不含Playback。jobs的job_defs/jobs/job变化与保留游标按当前构造器保留。library的恢复表保留原执行语义：pending删除、scan checkpoint、Probe计数/partial/阶段退避、fingerprint samples/models/attempts、章节帧、Library removal边界；仅file相关列统一 `ledger_id→file_id`、`ledger_probe_obligations→file_probe_obligations`。
- fingerprint/Probe/章节历史行不简单级联消失：调用现有取消/退休/计数收口再删除文件依赖，保留terminal Job定义/状态合同。Media markers跨Library共享，最后引用前不得清掉；历史/会话中的file引用故意没有FK，文件删除不删观看事实。

### DDL约束之外的领域校验

SQLite CHECK不能表达跨表kind、复杂JSON的全部语义；事务入口必须检查：TV覆盖坐标非负，movie/video只有whole；root路径组件级归属与唯一最长根；单位/范围不溢出i64；JSON字段类型/语言/来源枚举；优选文件published/present且源版本匹配；default Library与最后同kind保护；ACL名单真实User校验。延迟FK只允许同事务重绑定顺序，不允许跨提交暂时不一致。

字段规范：
- UUID文本规范小写带连字符；公开逻辑ID由TypedItemId编码，不以字符串前缀猜对象。
- locale标准化BCP47大小写，fallback顺序显式；source小写固定枚举。
- normalized_json/details_json是标准化冷payload，拒绝未知版本；类型标签`version:1`。原始HTTP body只在cache。
- FieldValue编码 `{"state":"value","value":...}` / `{"state":"empty"}`；对象中无键=Missing。present_fields_json保存对应字段集合，必须与payload相符；Empty与Missing分离。field_origins每字段记录source、provider_entity_id、source_revision、origin_media_id、presence以及pending_target_fallback/target_external_id，不能只写来源名。
- 已采用metadata的nullable SQL列只供存值，缺失/显式清空由field_origins/presence区分。只有经过字段校验的值可参与fallback；rating 0..10、runtime>=0、日期合法ISO日期。无效输入不是显式空。
- library_items.title用本Library选定locale/NFO后显示值；media.title是基本身份名称，不作为全部语言的最终显示。title_sort保持现有Rust字符串字节排序；alias_json为固定五来源外部ID摘要，展示译名仍在media.aliases_display_json，不混用。
- added_at_sort对NULL使用i64::MIN，非NULL为纳秒；size/runtime排序NULL映射0、rating映射-1、release_date为空串或年兜底日期；ASC/DESC与旧行为相同。cursor tie按方向同向ID，offset继续旧ID升序tie；shuffle取现有稳定算法，不用进程随机hash seed。
- watch_activity_at/favorite_at/last_played_at维持既有秒单位，played_changed_at_ms为新毫秒时钟，禁止混比；相同played时钟按episode>season>whole。事件新鲜性由session generation/EventSequence负责，不能靠墙钟取代。
- FileVersion是文件内容/STRM URL版本，不因只刷新海报/质量字段变化而增加；tracks_version单独推进。元数据/摘要revision更新不能让未变视频丢恢复资格。Resume原源精确匹配，不把同长度或同路径当等价。

响应缓存公开接口（T2）：`CacheSource`固定六枚举、as_str/table_name返回静态白名单；`CacheRow { source: CacheSource, entry_id:i64, cache_key:String, body:String, fetched_at:i64, expires_at:Option<i64> }`；`CacheDbError`为Sqlite/Io/UnknownSource/LayoutMismatch。`open(path:&Path)->Result<Connection,CacheDbError>`；`get(conn:&Connection,source:CacheSource,key:&str)->Result<Option<CacheRow>,CacheDbError>`；`put(conn:&Connection,source:CacheSource,key:&str,body:&str,fetched_at:i64,expires_at:Option<i64>)->Result<(),CacheDbError>`；`delete(conn:&Connection,source:CacheSource,key:&str)->Result<bool,CacheDbError>`；`list(conn:&Connection,source:Option<CacheSource>)->Result<Vec<CacheRow>,CacheDbError>`；`details_for_prefixes(conn:&Connection,source:CacheSource,prefixes:&[String])->Result<Vec<CacheRow>,CacheDbError>`。初始化器事务执行六表DDL，仅空目标布局允许；TTL/端点校验/stale由原CatalogCache复用实现，不放进Store。

## 2. 公开类型：domain只放事实，不引入IO

在 `crates/domain/src/library_storage.rs` 新建并由lib.rs导出。`LedgerId`仍是稳定文件台账UUID类型，**不新增等价FileId类型/别名**；所有新字段统一名file_id。旧LedgerRow及只有单一episode的构造器在全调用方切换后删除，不以shim继续用。

```rust
pub struct UnitKey { pub season: i32, pub episode: i32 } // whole(-1,-1), season(s,-1), episode(s,e)
pub enum UnitScope { Whole, Season(u32), Episode { season: u32, episode: u32 } }
pub struct FileVersion { pub revision: i64, pub source_version: String }
pub struct LibraryFile {
    pub file_id: LedgerId, pub library_id: LibraryId, pub root_id: String,
    pub media_id: MediaId, pub kind: MediaKind, pub path: std::path::PathBuf,
    pub version: FileVersion, pub presence: FilePresence, pub publication: FilePublication,
    pub size_bytes: Option<u64>, pub stat_known: bool,
    pub created_ns: Option<i64>, pub modified_ns: Option<i64>, pub source_digest: Option<String>,
    pub source_path: Option<std::path::PathBuf>, pub transfer_mode: Option<String>,
    pub resolution: Option<String>, pub codec: Option<String>, pub hdr: Option<String>,
    pub quality_source: QualitySource, pub confidence: Confidence, pub filter_score: Option<i32>,
    pub release_quality: Option<String>, pub duration_ms: Option<i64>, pub tracks_version: i64,
    pub poster_path: Option<std::path::PathBuf>, pub backdrop_path: Option<std::path::PathBuf>, pub still_path: Option<std::path::PathBuf>,
}
pub enum FilePresence { Present, Missing, Unknown }
pub enum FilePublication { Pending, Published }
pub struct PlayedAssertion { pub value: bool, pub changed_at_ms: i64 } // Option=None => no assertion
pub struct ResumeBinding {
    pub file_id: LedgerId, pub version: FileVersion, pub timeline: TimelineMode,
    pub position_ms: i64, pub duration_ms: Option<i64>, pub tracks_version: Option<i64>,
    pub audio_track: Option<String>, pub subtitle_track: Option<String>,
}
pub enum TimelineMode { FullFile }
pub enum EntityScope { Work, Season(u32), Episode { season: u32, episode: u32 } }
```

所有类型derive按实际边界所需（Copy仅小枚举/键，Clone不要大对象无意义复制）；serde只对需要跨crate/JSON边界者启用workspace依赖。LibraryFile不把覆盖数组嵌入自己；批量发布显式传UnitKey切片。

## 3. Store原子接口：归约函数不公开原始SQL事务

新 `crates/store/src/library_write/{mod.rs,publish.rs,reduce.rs,presence.rs}`。所有入口返回StoreError，新增 `Conflict { entity: &'static str, reason: String }`、`StaleVersion`、`LayoutMismatch`、`Integrity`（thiserror）；HTTP分别409/409/启动错误/500，存储失败不得改成空列表。仅内部 `LibraryWriteTx`持有rusqlite Transaction和引用，避免嵌套BEGIN与一个写入口调用另一个自带事务入口。

```rust
pub struct WriteScope { pub library_id: LibraryId, pub config_revision: i64 }
pub struct FilePublication<'a> {
    pub file: &'a domain::LibraryFile, pub units: &'a [domain::UnitKey],
    pub expected_version: Option<&'a domain::FileVersion>, pub probe_required: bool,
}
pub struct PublishReport { pub inserted: usize, pub updated: usize, pub unchanged: usize }
impl Store {
    pub fn publish_files(&self, scope: &WriteScope, files: &[FilePublication<'_>], now_ms: i64) -> Result<PublishReport, StoreError>;
    pub fn publish_file_details(&self, file_id: LedgerId, expected: &FileVersion, details: &FileDetails, now_ms: i64) -> Result<(), StoreError>;
    pub fn apply_presence(&self, scope: &WriteScope, observations: &[PresenceObservation], mode: PresenceMode, now_ms: i64) -> Result<PresenceReport, StoreError>;
    pub fn begin_transfer(&self, scope: &WriteScope, reservation: &TransferReservation, now_ms: i64) -> Result<LibraryFile, StoreError>;
    pub fn finish_transfer(&self, scope: &WriteScope, publication: &FilePublication<'_>, now_ms: i64) -> Result<(), StoreError>;
}
```

`FileDetails`在store的files模块，字段对应SQL冷表（source_revision/source_version/tracks_version，typed video/audio/subtitle/chapters，fingerprint/outro及format_duration_ms），复用现有library探测类型，不引入第二套stream结构。`PresenceObservation`包含file_id、observed_version、observed_path、presence；mode=Verify/Prune，report=missing/restored/deleted/skipped。`TransferReservation`含LibraryFile(pending)、唯一source_path与mode；同path不同身份/来源返回Conflict；不能upsert覆盖他人保留行。

发布一次最多100文件，先验全部输入再开事务；内部收集变动Media去重，更新文件/coverage/units/items/terms/受影响用户摘要及Probe obligation，任何错误全窗口回滚。Stat/NFO/网络在事务外；同内容未变化不推进file_revision、不重新建义务。内部reduce只按索引读受影响Media，当前心跳绝不调用全用户reduce。缺失/未知、无分集编号、pending都有单独行为测试。

### Transfer跨crate传输合同（T5拥有）

`crates/subscribe/src/{collect,types,collection_destinations}.rs`与API worker/transfer同批改，references继续枚举目的/恢复消费者，不引入Store依赖。domain新增 `FileCoverage { file: LibraryFile, units: Vec<UnitKey> }`；RunInput显式带library_id/root_id/config_revision；RunOutcome以 `files: Vec<FileCoverage>`替换ledger，保留facts/removed_paths/成功失败等其余字段。LedgerSource改为 `FileSource { file_id: LedgerId, destination: PathBuf, source: PathBuf }`。

Recorder签名：`prepare(&self, candidate: &FileCoverage, source: &Path, mode: library::TransferMode) -> Result<FileCoverage, SubscribeError>`；`published(&self, candidate: &FileCoverage) -> Result<(), SubscribeError>`。覆盖从每个文件Release在发布前解析，随prepare/published/outcome完整传递；整季torrent不能让每个单集文件继承整季覆盖。Probe后质量在最终published前写入candidate，API recorder分别调用begin_transfer/finish_transfer。测试多集文件两单元、同路径恢复不丢E02、拒绝替换不提前删旧覆盖；失败保留其他已成功文件真实结果。

## 4. 元数据/NFO采用接口

在store `metadata/{mod.rs,types.rs,adopt.rs,merge.rs}`，在library `nfo/{read.rs,fields.rs}` 拆现有nfo.rs；media只负责外部来源解析及响应缓存，不依赖Store。

```rust
pub enum FieldValue<T> { Missing, Empty, Value(T) }
pub struct NormalizedMetadata {
    pub fields: MetadataFields,
    pub field_origins: std::collections::BTreeMap<String, SourceFieldOrigin>,
}
pub enum SourceSelection { Direct, Translation, Certification, Empty }
pub struct SourceFieldOrigin {
    pub selection: SourceSelection, pub response_pointer: String,
    pub actual_locale: Option<String>, pub certification_country: Option<String>,
}
pub enum NfoReadOutcome { Missing, Unknown { message: String }, Invalid { message: String }, Valid(NfoInput) }
pub struct MetadataAdoption {
    pub media_id: MediaId, pub scope: EntityScope, pub locale: String, pub source: String,
    pub provider_entity_id: String, pub provider_work_id: String, pub fetched_at: i64,
    pub normalized: NormalizedMetadata, pub expected_revision: Option<i64>,
    pub preferences: MetadataPreferences,
}
pub struct NfoInput {
    pub scope: EntityScope, pub locale: Option<String>, pub fields: MetadataFields,
    pub work_aliases: Vec<MediaAlias>, pub source_version: String,
}
impl Store {
    pub fn adopt_metadata(&self, input: &MetadataAdoption, now_ms: i64) -> Result<i64, StoreError>;
    pub fn ingest_nfo(&self, scope: &WriteScope, path: &std::path::Path, media_id: MediaId, input: &NfoReadOutcome, now_ms: i64) -> Result<(), StoreError>;
    pub fn set_metadata_overrides(&self, media: MediaId, scope: EntityScope, locale: &str, fields: &MetadataFields, expected_revision: i64) -> Result<i64, StoreError>;
}
```

MetadataFields/FieldValue放domain/src/metadata_fields.rs，serde继承workspace，media/library/store共同消费；不新增反向依赖。每字段为FieldValue<T>：String字段title/original_title/overview/tagline/release_date/end_date/content_rating/language/status/air_date/poster_url/backdrop_url/still_url；year:u16、rating:f64、vote_count:u64；runtime_minutes/number_of_seasons/number_of_episodes:u32；countries/studios/directors/creators:Vec<String>；**genres:Vec<MetadataGenre>**；episode_run_time:Vec<u32>；cast:Vec<MetadataCast>。`MetadataCast { name:String, role:Option<String>, tmdb_id:Option<String>, thumb:Option<String>, order:Option<u32> }`，缺order用来源数组位置。MediaAlias=source/kind/external_id，episode ID不当work身份。

details_json version=1，冷键tagline/end_date/content_rating/status/vote_count/studios/directors/creators/number_of_seasons/number_of_episodes/episode_run_time/cast，每键独立presence/provenance，不能把整个details当一个覆盖值。热字段存对应metadata列，season/episode的其他冷字段也用该对象；episode air_date/still_url映射专列。Empty与Missing分离，nullable列不丢field_origins中presence。

映射：NFO plot→overview、premiered→work.release_date、aired→episode.air_date、thumb→episode.still_url或work/season.poster_url（按scope）、actor→cast、studio/director/creator→对应集合、content_rating/end_date/status/vote_count/counts→同名字段；season/episode为坐标不改work名。TMDB last_air_date→end_date、original_language→language、companies/networks→studios、credits→cast/directors/creators、episode.name/still_path→title/still_url。API原info、Jellyfin Overview/PremiereDate/OfficialRating/Studios/People/SeriesStatus、NFO导出从这些采用字段映射，不在HTTP补读原NFO/cache。

**presence与选择来源不得先丢后补**：T3新增media/src/normalized.rs入口 `normalize_metadata(source: &str, scope: EntityScope, body: &str, preferences: &MetadataPreferences) -> Result<NormalizedMetadata, TmdbError>`，一次返回字段与逐字段选择证据。原始键不存在→Missing、null/空文本/空数组→Empty、合法类型→Value、非法值→本次响应错误不覆盖旧值；来源明确null为Missing的例外有fixture。采用链不得从有损ItemMeta/EpisodeMeta逆推presence或来源；NFO保留出现性，自己的path/locale来源由NFO摄取合同处理。

上面FieldValue/MetadataFields/EntityScope/TypedItemId等共享声明只在domain定义一次，store/library/media通过use引用，不各复制同名类型。NfoReadOutcome/NfoInput定义在library的NFO模块，用domain MetadataFields/EntityScope；Store已有library依赖可直接消费。normalize_metadata引用同一domain类型，不引入media→library/store依赖。

Scrape导出先写library_operations(kind=scrape)意图（原文件版本、输出digest、字段源和export_id），原子文件替换后同事务确认输入/导出记录并更新摘要，再完成意图；恢复确认digest再ACK。Unknown/Invalid保留最后有效字段，不解除锁；确认删除移除该输入；自产未改输出不升级权威。refresh只改被明确采用的本地数据，直接cache.put不触发采用。

### 显式采用偏好（集成复审F02）

domain定义 `MetadataPreferences { language_priority: Vec<String>, cert_country_priority: Vec<String> }`，API metadata_service从本次已校验的Scrape配置快照生成并显式传入normalize与MetadataAdoption；不得从纯解析器隐式读全局配置。locale是该采用行的实际显示语言，language_priority是此次来源翻译选择/回退顺序，地区认证优先级独立于locale，不能从zh-CN猜成只能用CN认证。默认列表复用现有scrape_config默认值，不新建第二套默认。

TMDB内容评级按当前selected_content_rating合同：电影先按响应国家/发布顺序取各国首个非空certification，TV用content_ratings的非空rating；按cert_country_priority找首个可用，再以响应中首个可用兜底。translations的overview/tagline按language_priority的地区精确→同语言顺序补缺；这是来源内部明确fallback，不与跨来源Missing/Empty合成混淆。原字段存在且最终无可用内容时为Empty，字段及替代结构均未出现为Missing；选中值记录实际语言/国家来源，不能把fallback翻译标成请求原文。

来源快照保存preferences_json（两优先列表）及selection_origins_json（NormalizedMetadata.field_origins），再与normalized_json/present_fields_json、source_revision同事务提交。有效元数据field_origins_json在选中字段的原SourceFieldOrigin上加source/provider_entity_id/source_revision等身份，不丢掉实际locale/国家；采用失败时值与来源一并回滚。修改偏好不自动重解释旧记录，明确刷新重新解析原始响应并采用；清cache/重启不改旧值及来源，人工锁/NFO仍优先，不能从有损ItemMeta或偏好列表猜实际选择。

验收：同一真实fixture按默认与[GB,US,CN]分别得到既有默认评级和15；同一language优先列表得到现有法语overview/中文tagline回退。将结果采用→清cache→重启，本地详情/NFO/Jellyfin OfficialRating一致；缺结构、空结构、显式null分别验证presence，失败刷新不覆盖旧记录。

#### 逐字段来源传输与编码（F02-R1）

`NormalizedMetadata`、SourceFieldOrigin、SourceSelection在domain的metadata_fields模块定义一次。Map键严格使用MetadataFields已声明的字段名；Value/Empty各须恰有一个origin，Missing不得有origin，未知键/缺证据在采用边界返回Invalid并整次回滚。`response_pointer`是实际选中原始JSON节点的JSON Pointer（含数组下标），不是整份响应body；Translation记录所选条目的iso_639_1/iso_3166_1实际locale，Certification记录所选条目的实际国家。不选择任何值的Empty指向显式空字段/空候选容器，国家与locale为None，不伪造偏好第一项。

Direct字段若响应没有可靠语言标识则actual_locale=None，表示未声明，不能把original_language、请求locale或偏好顺序冒充字段语言。翻译候选具有语言/地区时保留实际值；同语言无地区信息只记语言，不补成请求地区。整行MetadataAdoption.locale仍是存储/展示槽位，不代表所有字段来源语言相同；一个overview与tagline来自不同语言的行必须能分别解释。选择规则执行时就创建证据，API以input.normalized整包传Store，不取出fields再重建丢origin的对象。

selection_origins_json编码为 `{"version":1,"fields":{"content_rating":{"selection":"certification","response_pointer":"/content_ratings/results/0/rating","actual_locale":null,"certification_country":"US"}}}`；normalized_json仍存标准化字段，preferences_json仅是输入偏好，不能代替选择结果。effective field_origins_json对应字段保存同一selection对象（加provider/source版本与原身份）；metadata合并、pending fallback、checker重算、Scrape导出来源登记必须复制这份证据，不因显示值未变省略来源更新。源快照字段值相同但origin不同仍推进source_revision及其派生来源；只在同值同源时允许no-op。

验收：同偏好[GB,US,CN]，仅US=15与仅GB=15的两响应都显示15，但持久snapshot/effective origins分别US/GB；fr-FR请求同一文本仅fr-CA可用时记fr-CA而非fr-FR；相同文本在两个翻译条目中按优先选择后pointer/locale不同。Value/Empty/Missing覆盖源Map一致性，来源缺失/未知字段拒绝；采用→清cache→重启→checker/合并/导出后来源不丢，失败刷新保旧值及旧origin。T3负责parser/采用/SQL编码，T5负责导出，T9负责合并/checker原样搬运；不新增表。

### Genre身份、规范筛选键与展示（集成复审F03）

domain定义 `MetadataGenre { source: String, source_id: Option<String>, name: String, filter_key: String }`；genres整体仍为FieldValue<Vec<MetadataGenre>>，ID和名称作为一份值一同选择/覆盖/provenance，不能把ID从TMDB、名称从另一来源拼成假对应。metadata.genres_json存version1对象数组，JSON值里保留上述四字段；有效字段origins记录采用locale/source，详情按name输出，NFO输出name，Jellyfin Genres输出name并保留原名称UUID GenreIds协议。

TMDB有效genre数字ID直接成为十进制filter_key，例如source_id=28/name=Acción→filter_key="28"；不同语言名称不影响该键。NFO无ID及其他来源先用现有中英文genre映射表找规范TMDB键（Action/动作→28）；不得将其他来源自己的数字ID冒充TMDB数字。无法映射时 `filter_key="name:"+规范Unicode小写名称`，source_id保留原来源值或None，name保原展示文本，不猜未知西语名为28。若原始响应含明确TMDB ID则优先用ID，不能丢ID再靠名称猜。

library_item_terms.term_kind=genre用filter_key，facets.value为同一键、label取本Library采用name；同key多个展示别名只计该Media一次。Web library-filter-bar及web/lib/api/libraries的genre选择改为规范字符串键（数字输入统一转十进制字符串），移除Number(value)以支持name:未知项；已存在g=28仍匹配，不把name:转NaN。公开详情原来输出名称数组处由adapter从对象映射name，不直接泄露结构改变旧DTO；数字GenreIds/名称生成UUID各走明确适配，与Library filter_key区分。

T3负责JSON/parser/NFO映射、T4负责terms归约、T7负责facets值/label去重、T8负责前端string与Jellyfin名称/UUID。验收：28的Action/Acción均命中g=28，label/详情/NFO保持相应语言；无ID Action/动作映射28，未知名称可按name:键筛选且前端不NaN；Missing/Empty/Value覆盖不分裂ID/name，不新增表。

## 5. 查询合同

新store `library_query/{mod.rs,types.rs,items.rs,units.rs,facets.rs,details.rs}`；API解析现有query到typed条件，不再在Rust整库排序。

```rust
pub struct LibraryQuery { pub library_id: LibraryId, pub user_id: Option<UserId>, pub filter: BrowseFilter, pub order: BrowseOrder, pub descending: bool, pub page: BrowsePage, pub include_total: bool }
pub enum BrowsePage { Offset { start: usize, limit: usize }, Cursor { after: Option<String>, limit: usize } }
pub enum BrowseOrder { Title, AddedAt, ReleaseDate, Rating, Runtime, Size, Resolution, Random, LastPlayed }
pub struct ItemPage { pub items: Vec<LibraryItem>, pub total: Option<u64>, pub next_cursor: Option<String> }
pub struct BrowseFilter {
    pub genres: Vec<String>, pub countries: Vec<String>, pub decades: Vec<String>, pub runtimes: Vec<String>,
    pub languages: Vec<String>, pub resolutions: Vec<String>, pub stock: Vec<String>,
    pub hdr: Option<bool>, pub min_rating: Option<f64>, pub identity: Option<String>,
    pub watch: Option<String>, pub unwatched_first: bool, pub require_image: bool,
}
pub enum FacetTier { Primary, All }
pub struct FacetRequest { pub library_id: LibraryId, pub user_id: Option<UserId>, pub filter: BrowseFilter, pub tier: FacetTier }
pub struct LibraryFacets { pub total: u64, pub groups: std::collections::BTreeMap<String, Vec<FacetCount>> }
pub struct FacetCount { pub value: String, pub label: String, pub count: u64 }
pub struct FilePage { pub files: Vec<LibraryFile>, pub next_cursor: Option<String> }
pub struct LibraryItemDetail { pub item: LibraryItem, pub metadata: MetadataFields, pub files: Vec<LibraryFile>, pub next_file_cursor: Option<String> }
pub struct UnitPage { pub units: Vec<LibraryUnit>, pub total: u64, pub next_cursor: Option<String> }
impl Store {
    pub fn library_item_page(&self, query: &LibraryQuery) -> Result<ItemPage, StoreError>;
    pub fn library_facets(&self, query: &FacetRequest) -> Result<LibraryFacets, StoreError>;
    pub fn library_item_detail(&self, library: LibraryId, media: MediaId, locale: &str) -> Result<Option<LibraryItemDetail>, StoreError>;
    pub fn library_unit_page(&self, library: LibraryId, media: MediaId, season: Option<u32>, page: &BrowsePage) -> Result<UnitPage, StoreError>;
    pub fn library_file_page(&self, library: LibraryId, media: MediaId, after: Option<&str>, limit: usize) -> Result<FilePage, StoreError>;
    pub fn visible_item_page(&self, user: UserId, libraries: &[LibraryId], filter: &BrowseFilter, order: BrowseOrder, descending: bool, page: &BrowsePage, include_total: bool) -> Result<ItemPage, StoreError>;
}
```

BrowseFilter完整保留g/c/d/rt/lang/res/stock、hdr/rating/identity、watch/未看优先及gallery图片条件；同类OR、跨类AND，所有参数绑定。BrowseOrder方向、null映射、stable tie严格按主设计；`probing`保留现有按title回退，不能杜撰额外索引列。LibraryItem字段覆盖DDL卡片列及当前User摘要，不含完整files；detail可返回有界文件页与后续cursor，不偷偷删旧响应文件。现有必须完整返回的协议按批次分页取齐并承认成本。

LibraryItem在store/library_query/types.rs逐列映射library_items（typed Media/Library IDs、bool、Option标量、完整计数/排序值及可选UserItemSummary），UserItemSummary逐列映射user_library_items；LibraryUnit逐列映射library_units并包含UnitKey。SQL列到类型是唯一字段清单，不再手写第二份facts_json结构；新增字段必须同步DDL/mapper/序列化消费者。LibraryItemDetail首批文件上限50，旧HTTP需要完整files时adapter继续取文件页直到齐全，不默默截断；新cursor API可返回分页标识。空授权Library集合返回空页，跨库visible_item_page按当前ACL复核输入集合并在SQL去重后分页。

page/count/facets同一短读事务；权限在分页/计数前用当前Library配置检查。跨Library查询在SQL授权域内去重再分页，不在各库先取一页合并。cursor为version/Library/User/排序方向/filter指纹/末排序键/ID的base64编码，解码时与请求严格匹配（不是安全token，授权仍重查）。terms/用户状态索引关联是允许的例外；精确count、包含搜索和复杂观看排序不宣称恒定时间。

分面保留排除自身维：total施加全部filter；维D候选来自授权Library全集（含count0），count为除D外条件满足且含D值的distinct Media数；primary=genre/country/decade/watch、all十维，years输出空数组。Genre28+CountryJP时total1，US仍按不施加country条件计数，T7沿facets_regression验收。

续页由T4提供file索引读、T7暴露library_file_page：ORDER BY file_id ASC，after含version/Library/Media/末file_id并验证scope，limit1..100，detail首批50；files_library_media索引只读published，missing仍展示。51文件回归必须完整唯一，旧HTTP adapter逐页取齐不截断；单个完整HTTP枚举用同短读快照或明确版本冲突重试，不能拼两代结果。跨独立请求不保证冻结快照。

### 协议查询完整输入与 SQL 执行（集成复审F01）

不能把Jellyfin筛选压成只有海报墙字段的BrowseFilter。T7在domain定义协议无关的typed请求、Store在 `library_query/protocol.rs` 实现，T8把现有ServerItemQuery逐字段映射，media-server不依赖Store。具体接口：`Store::protocol_item_page(&self, request: &ProtocolItemQuery) -> Result<ProtocolItemPage, StoreError>`。

`ProtocolItemQuery { user_id: UserId, parent: ProtocolParent, series_id: Option<MediaId>, season: Option<(MediaId,u32)>, person_ids: Vec<String>, studio_ids: Vec<String>, search_term: Option<String>, genres: Vec<String>, genre_ids: Vec<String>, filters: Vec<ProtocolMarkFilter>, is_favorite: Option<bool>, is_played: Option<bool>, include_types: Vec<ProtocolItemType>, recursive: bool, is_folder: Option<bool>, sort: Vec<ProtocolSort>, offset: usize, limit: usize }`；`ProtocolParent = Root | Library(LibraryId) | Series(MediaId) | Season(MediaId,u32) | Folder { root_id:String } | Genre(String) | Studio(String) | Unknown`；ItemType=Movie/Series/Season/Episode/Video；MarkFilter=Favorite/Played/Unplayed，未知filter沿原行为无附加谓词，不新增Resumable能力。`ProtocolSort { key: ProtocolSortKey, descending: bool }`，Key=Name/DateCreated/SeasonNumber/EpisodeNumber/Noop。`ProtocolItemPage { items: Vec<ProtocolItemRef>, total:u64 }`，`ProtocolItemRef { library_id:LibraryId, item:TypedItemId, preferred_source:Option<PhysicalSourceId> }`；只对选中页补详情，不再过滤改变total。

映射和范围：parent先在当前授权Library/根范围解析，未知parent返回空，不退化全库；SeriesId/SeasonId解析typed身份，限制该作品/季的真实拥有单元。IncludeItemTypes与recursive沿当前ServerItemQuery/ParentScope规则决定作品卡或分集展开，IsFolder判断series/folder而非文件是否存在；folder_id仍按既有root编码解析为root_id。Folder内同Media只从该根文件/覆盖归约child count、季数及完成状态，不拿整个Library摘要代替；逻辑分集多版本去重、跨库去重及授权全部发生在分页前。Root/混合类型结果用SQL UNION分支形成统一逻辑项目集合，不能分别取一页再拼。

SearchTerm保持trim后Unicode lowercase包含匹配title/original_title/overview；分集/季按其已采用字段及允许的work共享回退，不只搜卡片名。PersonIds支持既有People公开ID（忽略UUID连字符）或TMDB person ID，actor/director/creator均按现有role身份生成规则；StudioIds保留既有大小写不敏感名称及已发出的studio父ID解析。Genres/GenreIds同时支持来源展示名、既有名称生成UUID/prefixed ID和规范genre筛选键（见F03），不能把这三种ID混成一类。所有现行ServerItemQuery输入必须有映射；未知sort key沿原实现无比较权重，已知多键按请求顺序和各方向，缺方向沿最后方向/既有缺省，稳定尾键MediaId再逻辑坐标，保证分集不因同Media平手乱序。

SQL实现：普通库墙仍items单表；协议复杂条件允许关联同Library/Media及locale的本地采用metadata、NFO有效输入和季/集行，用SQL谓词及JSON成员EXISTS在LIMIT/count前完成。T3的字段合成纯函数可注册为确定性SQLite scalar函数（rusqlite启用functions特性，domain/library纯函数无IO）来保持local NFO/locale优先级；Unicode lowercase/person及studio名称ID也用统一纯函数，不以SQLite ASCII lower/NOCASE偷换语义。禁止先把整库JSON解码到Rust集合过滤；SQL内部按当前授权范围扫描/解析冷字段是明确允许的必要关联成本，需计量，不承诺person/包含搜索索引seek。首版不新增people/studio倒排表、不将长简介复制进items；后续索引优化要有测量证据。

SortName/Name保持协议Unicode lowercase排序，与Web title字节排序分开；DateCreated沿当前文件created/mtime归约，SeasonNumber/IndexNumber用逻辑坐标。选择、去重、排序、filtered total在同一读快照，协议默认限制/clamp保持；冷字段关联失败返回错误，不能当人物未出现。T7负责SQL和全部字段语义，T8负责Parent/ID/协议解析和快照水合。验收：人物/studio/仅简介命中的项目排在未筛选第51位时，Limit=1仍正确返回且TotalRecordCount=1；同时覆盖Series/Season/Folder/IsFolder、复合排序、局部root计数、Unicode和Unknown parent。

## 6. Playback、逻辑ID与合并接口

新store `playback/{target.rs,mutate.rs,resume.rs,summary.rs}`；保留sessions/history/stats/retention模块，连接从subscribe切library。API/provider共用同一个target和resume入口。

```rust
pub enum TypedItemId { Work(MediaId), Season { media: MediaId, season: u32 }, Unit { media: MediaId, key: UnitKey } }
pub struct PlaybackTarget { pub media_id: MediaId, pub unit: UnitKey, pub file: LibraryFile, pub config_revision: i64, pub timeline: TimelineMode }
pub enum ResumeStatus { Compatible, SourceChanged, NoProgress, AlreadyPlayed }
pub struct ResolvedResume { pub target: PlaybackTarget, pub start_ms: i64, pub duration_ms: Option<i64>, pub audio_track: Option<String>, pub subtitle_track: Option<String>, pub status: ResumeStatus }
pub struct PlaybackMutation { pub user_id: UserId, pub scope: UnitScope, pub played: Option<bool>, pub favorite: Option<bool>, pub expected_generation: Option<i64> }
pub struct PhysicalSourceId { pub file_id: LedgerId, pub revision: i64 }
pub struct MediaMergeRequest { pub request_id: String, pub survivor: MediaId, pub loser: MediaId, pub survivor_revision: i64, pub loser_revision: i64, pub refresh_targets: std::collections::BTreeMap<String,String> }
pub struct OperationStatus { pub operation_id: String, pub phase: String, pub state: String, pub error: Option<String> }
impl Store {
    pub fn resolve_playback(&self, user: UserId, item: &TypedItemId, preferred_source: Option<&PhysicalSourceId>, explicit_start_ms: Option<i64>) -> Result<ResolvedResume, StoreError>;
    pub fn mutate_playback(&self, media: MediaId, change: &PlaybackMutation, now_ms: i64) -> Result<(), StoreError>;
    pub fn commit_playback_event(&self, event: &BoundPlaybackEvent, now_ms: i64) -> Result<EventOutcome, StoreError>;
    pub fn begin_media_merge(&self, request: &MediaMergeRequest, now_ms: i64) -> Result<String, StoreError>;
    pub fn advance_media_merge(&self, operation_id: &str, now_ms: i64) -> Result<OperationStatus, StoreError>;
}
```

TypedItemId具体wire：`w1_<uuid无连字符>`、`s1_<uuid>_<season>`、`u1_<uuid>_<season>_<episode>`；非负数禁止溢出，whole unit使用-1/-1。工作级movie/video Work自动解析whole，TV Work用于Series，不冒充某集。MediaSourceId=`f1_<file_uuid无连字符>_<revision>`；旧路径中的ledger_id只是参数名，所有URL/前端一起生成新语义，不保留旧file-ID当episode的alias。原Web body的(0,0) movie在边界映射whole；TV S00E00保留。

BoundPlaybackEvent含user/device、session_id、generation、event_sequence、typed logical item、source id/version、position/duration/track、paused/rate/client信息及Playing/Progress/Stopped种类；EventOutcome=Applied/IgnoredStale（原协议应答不变，状态不变）。Start持久绑定逻辑/物理源/Library配置版本；解析不删旧恢复包；未完成心跳不凭空创建新的false断言，首次达到完成阈值写true@now_ms；人工mark/unmark写true/false断言；重播已看保留逻辑标记直至显式unmark，favorite/track不写断言。会话终态历史按稳定session键幂等，时间推进按现有墙钟/rate限制，不把seek累计为观看。

MergeRequest含survivor/loser、各冲突source的refresh_target_external_id、expected media revisions、request_id幂等键；后端操作保存稳定阶段与逐字段plan。重试同request_id必须返回同operation（plan_json持久保存request_id，生产DDL需以operation_id=request_id规范UUID作为唯一键，不额外建第二个幂等表）。PlayedAssertion合并有效值OR/max时钟；恢复包按真实活动整包选；fallback固定survivor字段优先、Missing/Invalid才用loser、Empty保留，真实来源pending保存field_origins。合并禁止同时进行冲突身份操作，冻结后排空，跨subscribe/jobs引用逐步收敛；每步恢复必须有入口，不只声明表存在。

T8还拥有独立playback crate：`crates/playback/src/dto.rs`、lib.rs、tests/jellyfin.rs；构造器改 `media_source(file: &LibraryFile) -> MediaSource`（编码file/revision）与 `Item::from_target(logical_id: &TypedItemId, name: &str, file: &LibraryFile) -> Item`（编码逻辑ID）。TypedItemId纯编码器放domain，DTO不依赖api/store；删除from_ledger/旧compact file-ID作为Item ID。两个逻辑分集共享一文件时Item.id不同、MediaSource.id相同，作为消费者可见行为测试。

## 7. 后台/管理操作与删除规则

- `library_operations`保存roots/locale/integrity/scrape明确状态机；唯一活跃scope由应用事务预检，operation_id稳定。状态planned→frozen/draining→applying/checking→succeeded或failed（failed保留恢复阶段和cursor）。不允许普通请求内全库重建。
- 保留已有Library删除202/status、freeze/drain/lease/worker阻止、root边界tombstone、末库保护和显式Subscribe绑定不可用。根/locale operation期间API返回409 `library.reindexing`并带operation_id，管理员状态页可重试。
- 添加管理员 `POST /api/v1/media/merges`、`GET /api/v1/media/operations/{id}`、`POST /api/v1/libraries/{id}/integrity-checks` 与 `POST /api/v1/libraries/{id}/integrity-repairs`，返回202 operation_id；复用GET Library operation状态路由（新增 `/api/v1/libraries/{id}/operations/{operation_id}`）。不为查询暴露原始SQL或未校验表名。
- 重新识别仍走现有 `/reidentify`，只移动显式文件及覆盖；不是调用merge。metadata人工字段修改新增管理员 `PUT /api/v1/media/{id}/metadata-overrides`，要求entity/locale/expected_revision；冲突409。
- checker从文件/coverage和原始用户事实驱动并反查孤儿，每批<=100 Media，单Media内有界读/版本重检；repair按单Media写事务重算。检测前普通页不能保证发现所有漏行，检测后integrity=failed阻止假成功。
- 历史清除保留既有收藏合同；清观看需清恢复包、play_count/活动/played断言并重算摘要；只删日志的保留Job不得清这些事实。file prune/removal更不能调用User历史清除。
- app User删除跨DB：先在app.settings持久写版本化 `user_removal.<user_id>`（operation_id/阶段/cursor/错误），禁用User并revoke token→排空该用户session→清library用户事实/私有Collection→清subscribe与typed jobs引用→最后删User→完成标记。每阶段幂等、app提交后才能跨库推进，启动worker先恢复未完删除；不新增通用事件总线，认证检查disabled要覆盖全部协议。failure保阶段可重试，不让凭证恢复为可用；设置行按既有历史维护政策处理。
