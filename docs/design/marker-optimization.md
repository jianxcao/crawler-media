# 片头片尾识别、章节图像与性能设计

片头片尾优化同时处理文件时间线、识别证据、发布、章节图像和重复工作的成本。减少采样和比较必须保持同一套准确性要求。重复音频或成功生成标记本身不能证明该区间可安全跳过。

本设计描述当前实现及其边界；真实准确性、性能配对和页面验证方法见[标记验收手册](../testing/marker-accuracy-runbook.md)。存储与锁遵循[数据与存储性能设计](storage-performance.md)，测试结果应另行记录，不由本文推断。

## 管线与结果身份

采集、分析、标记发布和图片渲染分别记录状态与耗时：

1. 从当前文件版本准备有界采样窗口，记录真实源和音轨证据。
2. 复用有效采样，比较跨集指纹，形成候选和季模板。
3. 验证当前目标与独立参考的时间映射、候选上下文及边界冲突。
4. 按 Intro/Outro 分别归约结果，经版本与生命周期屏障发布标记及章节义务。
5. 异步生成当前章节图像，持久记录每个目标的成功、失败与重试。

正式标记绑定 `(file_id, FileVersion, UnitKey)`。同一 Media/季/集的不同剪辑、音轨或源文件不能共享未经验证的时间。季模板可以复用，但每个目标仍需在自己的时间线上验证。多单元文件没有可靠单元时间映射时，自动检测不能推断各集边界。人工 locked 标记保护当前文件版本；源变化不能继承另一时间线的标记。

## 候选质量与安全边界

full_window 与 adaptive 共用质量和边界政策。指纹候选需要有效有限分数、足够时长、跨集支持和所需上下文；静音、常量指纹、冲突候选或损坏时间线不能靠“更长”获得优先级。

当前采样政策包含匹配时长 15–240 秒、最大 score 4、至少三份独立 episode 的发现证据。目标确认需要两份独立其他集参考；同一集的多个窗口不能冒充多集支持。相互矛盾且证据相当的候选保持 uncertain。

自动结果区分 `verified`、`no_match`、`uncertain`，网络/计算/存储失败另外保留。`uncertain` 不是已确认不存在。候选局部上下文完整不代表整窗 PCM 完整，后者是 absence 判定的独立条件。

边界复核使用实际采样证据和源章节：

- 观察范围为候选两端各约 10 秒，guard 为候选前后各 3 秒与已证明媒体边界的交集。
- 没有源章节时，普通 RMS 转折、静音或单侧音频提案不能单独移动候选；参考充分、guard 完整且无独立冲突时保持原候选。
- 有源章节时，唯一章节与音频两端需在 1 秒内一致，只允许向原候选内部收缩。
- 目标与参考都需要真实 guard 覆盖。采样窗边缘不是物理媒体边缘；未知媒体结束点不能免除右侧 guard。
- 对白叠音乐、滚动字幕、静音或关键帧都不是“没有正片”的充分证据。

## 采集时间线与源时钟

所有区间以实际 `SampleWindow.start_ms` 和已证明的时间映射转换。片尾发布不能再次用总时长倒推出采样偏移。时间转换使用检查过的加减和区间边界，拒绝溢出与超出实际窗口的坐标。

采集记录音轨、PCM 时长、源起始时间、连续性和源事实。未知连续性保持未知，已知缺口/重叠保持无效；不能以总样本长度覆盖时间线矛盾。短 PCM 只有在连续起点已独立证明时才可能提供局部候选证据，不能由短总长推断只是缺少尾部。

AAC-LC、44100/48000Hz、双声道、1024 native samples/frame、已验证 ASC 和 Matroska 1ms 时钟的受支持子集使用量化时钟证明。其他受支持的细时钟路径保留各自证明规则，不把不支持的格式猜成已验证格式。

量化采集从同次 FFprobe 的包位置、frame 对应关系、native sample count、skip/discard 和 encoded bytes 取得源证据，只对已拥有的字节做本地解码。独立 block/lace 首锚约束同一 phase 交集，原始 tick 描述时钟精度，不能当作逐帧随意容差。证据最多 32MiB/16384 packets；超界、缺失、歧义或损坏明确拒绝，不增加第二次源读取来掩盖失败。

量化证明保留 origin 的上下界、native rate、source tick、包/block 数、实际样本数、payload 摘要和证明类型。候选、模型、参考和 guard 都取合法映射的保守交集并向内取整，不能强制归零或加固定秒偏移。

物理媒体开头允许的 guard 缺口仅来自已验证源时钟精度，必须同时具备：窗口从零开始，独立 header 的 format/所选音轨明确从零开始，origin 范围包含零且未超出 producer 证明的 tick 上界。该规则只判断头部 guard 是否充分，不改变区间，不赋予尾部同样特例，不把整窗不完整转成 `no_match`，也不放宽内部窗口、源损坏或章节冲突。

边界政策身份与 capture profile 分开。分析规则变化应重新分析有效原始证据；源、音轨或采集格式变化才使采样缓存失效。实际协议中的 mapping/profile 标识是校验内容，不是文档的历史验收锚点。

## Intro 与 Outro 独立发布

每种 kind 独立保存 decision、status、reason 和是否保留同源既有结果。整体结果可为全部 resolved、partial 或全部 uncertain；存在未解决 kind 时不能报告两种标记都完成。

一个 kind 的 `verified` 可以独立保存区间并产生真实章节图像义务。另一个 kind 未确定或缺失时，只能保留同一当前 `FileVersion` 已接受的值，并明确 `retained_previous`；保留不代表本次重新验证。`no_match` 清除已有 kind 前仍需满足严格 absence coverage。全部 uncertain 不重写 marker/chapters。

普通与显式 refresh、full_window 与 adaptive 采用相同归约规则。refresh 仍在单事务中提交准备好的结果集，复查目标、引用、claim、源、Library/root 和覆盖；原子性表示本次合并结果共同提交，不要求 Intro/Outro 同时确定。真实执行失败不能伪装为 partial 成功。

## 晚出现片头的有界采样

默认初始头尾窗口为 180 秒。adaptive TV 路径在无法形成或验证 Intro、且现有时间线证据有效时，按需探索前部未覆盖范围；full_window 保持配置的完整窗口策略，不自动切换采样模式。

adaptive Intro 搜索窗口为 `[0,180]`、`[120,420]`、`[300,600]` 秒，并受实际时长夹取。每窗不超过 300 秒，搜索不超过 600 秒。已有窄验证和回退窗口计入每 kind 最多三个窗口、四次尝试以及 120 秒采集进程截止，不能额外赠送预算。

窗口保持独立采集与真实 origin，不能拼接独立 demux 的 PCM 伪造连续证据。重叠窗口降低边界遗漏，但不保证最长 240 秒候选一定完整落在某窗；接缝长候选可保持 uncertain。

只采集 pending kind；重启恢复已有 attempt/sample receipts、预算和精确 sample ID，不因另一 kind 失败重采已完成 kind。预算耗尽记录 sampling limit，损坏或不支持的计划以 `invalid_capture_plan` 有限终止；Store busy 仍按可重试写入处理。

跨集主题不要求出现在相同绝对窗口。发现视图每个 episode 只采用一次真实 capture，默认最多前五个 eligible episode、每集三个窗口形成有界 anchor 集，不做整季多窗口的指数级笛卡尔组合。模型每 kind 数量有上限，稳定模型继续用于下一集的预测窗口。

同一 file/kind 的多窗结果先归约：完整 verified 优先，多个 verified 冲突则 uncertain；局部 `no_match` 不能证明整个搜索区域没有片头。已有有效 Intro 模型不能因另一 kind 或轮次未形成模型被删除。

## 缓存与计算复用

远程读取固定并发 1、冷却 500ms。提速来自复用有效证据、缩小窗口和减少重复分析，不增加源站并发。

采样缓存身份包含文件/源、音轨、PCM/算法 profile、精确窗口和时间映射；分析阈值属于独立政策。热缓存重分析应能只做本地工作，手动 refresh 不等于无条件重新远程采集。

四类原始比较消费者——整季决策、adaptive 建模、template 验证、边界复核——共享 ProbeManager 的有界 pair 缓存：

- 缓存绑定不可变 engine，跨 job/季最多 10,000 项、32MiB，按 LRU 淘汰，超大项绕过。
- endpoint 包含真实文件/源/profile、kind、窗口、audio/mapping、指纹 words 摘要及长度；pair 再绑定完整分析政策、精确 min/max 和原始比较算法身份。
- 参数顺序属于 key；原生比较反转参数可能选择不同 motif/score，反向比较是独立 key，不能交换一次结果充当另一次计算。
- 保存完整 typed pair decision，不保存整段 PCM、最终季结论或发布资格。
- engine、Store、IO 和 await 都在短缓存锁外执行。并发重复 miss 如实计数。

每次分析独立记录 computed/reused 和 miss CPU 时间。缓存命中后仍根据当前 PCM、timeline、RMS、chapters、模型、共识和 publication barrier 重算派生结果。24 集、两种 kind、稳定窗口且无淘汰时，ordered pair 共有 `2 × 24 × 23 = 1104` 种；这个数量只限定原始比较键，不能当作整条管线的实际计算或墙钟时间。

## 持久调度与发布屏障

采集完成所需阶段后，`probe_job_units` 原子交接至 Analysis，再释放采集工作窗口。独立 Analysis consumer 处理同 Media/season 的合并工作，采集不等待分析安静期。

分析准入和普通 claim 使用 16 个工作项的有界 horizon。安静期 2 秒、持续变动最多等 10 秒；已到期且引用未变的后续页不重复等待。dirty context 保留实际首末变更时间与 generation，防止持续追加导致无限推迟。

目标、引用、元数据/音轨、采样载荷、源版本、Library/root/coverage 和分析政策共同构成上下文。read snapshot 只复制 owned raw 数据，释放 reader 后再解码、验证、哈希和编码。发布逐项比较实际使用的原始载荷与身份，不用数据库级全局版本号代替。

运行中输入变化拒绝过期结果，并重新排队仍存活的单元；无关追加不应使既有计算失效。普通 uncertain 是可观察终态，时间流逝本身不触发重采；新参考可为早期文件创建独立后续工作。显式跨季 refresh 收齐参与者后计算，再将准备好的各 kind 结果共同发布。

writer busy/locked 保留相同不可变 prepared commit/claim，只重试 checked publication。输入失效、非 busy 错误或关闭时才丢弃并安全重排。当前时长来自当前同源元数据，缓存旧时长不能证明媒体末端。

## 章节图像的持久闭环

任务状态为 queued、running、completed、partial、failed、cancelled、superseded，每个 target 分别持久化。只有全部目标拥有当前版本的可用图片才能 completed。一个成功一个失败必须 partial；重试只补未完成目标，不能降低已有成功目标的状态。

generation 包含 file、source version、规范化 targets 和 renderer 身份。每次 claim 为未完成目标分配不可变 candidate artifact，绑定 attempt 和唯一 ID。图片路径带 generation、目标 index 和 artifact，避免同名覆盖。成功目标保存精确 published artifact，后续重试不替它分配新身份。

图片在唯一 staging 中写入，经源观察和 JPEG 验证后以不覆盖方式发布；这些 IO 在 DB lease、writer 和 registry/publication 锁外。随后短事务只采用仍匹配 running attempt、Library、源与 generation 的 candidate。旧 worker 晚到落盘不能替换当前图片。

恢复先将 running 改为 queued 并推进 attempt，保留已登记 candidate；锁外验证精确路径后，以 recovery fence 和当前身份 CAS 收养。已经重新 claim、cancelled 或 superseded 的任务不能复活。权威目标从 A 变为 B 再变回 A 时，创建新 task_id/attempt 身份；相同内容 generation 不授予旧 worker 发布权。

显式生成/恢复发现 published 图片缺失时，只按预期 attempt、generation 和精确 artifact 将仍匹配的目标改 pending，保留其他图片。清理先在 SQL 中退休精确身份，再锁外 unlink；不能按 generation/index 通配删除仍发布或待恢复收养的图片。

章节、指纹、TheIntroDB 候选及人工边界修改共用确保当前截图任务存在的入口。标记/章节与截图义务在 Library 事务中共同登记，启动恢复依赖持久义务，不依赖内存 enqueue 永久成功。没有有效 targets 时返回 empty。

生成 API 返回真实 task_id、generation、state 和 counts；有权限的状态 API 用于进度查看。前端仅在 queued/running 时每秒轮询，终态重取章节，切集/版本或卸载停止轮询，partial/failed 提供缺失数和重试。静态 `/chapters/*` 保持公开访问，URL 必须匹配当前 published artifact；已开始的不可变文件响应不承诺被后续 SQL 变更即时撤回。

章节进程上限保持 `MAX_CHAPTER_PROCESSES = 2`、`DISPATCH_WINDOW = 64`。

## 准确性与性能的证据边界

工程回归验证时间映射、缓存键、版本屏障、部分发布与截图恢复，不等于人工安全区间验收。当前重复音频、章节和有限音频上下文仍不能充分证明没有正片；原始端点精度目标和统计准确性需要独立标注继续验证。模型仅覆盖有界 seed 集、独立采集窗口有接缝、格式证明只覆盖明确子集，这些都是可见限制。

真实评价用 present/absent/ambiguous 标签、可安全跳过区间及 protected 正片区间；双人独立观看/听音频后裁定，以 Media 分组冻结开发/验证/留出集。没有标签、样本不足、未冻结政策或合成样本不能报告真实准确性通过。

分别报告 Intro/Outro 的 precision、recall、有符号端点偏差、严重误跳、未决/错误和处理覆盖率。零预测不能靠零误报通过；定向挑选或反复调参看过的样本不再是独立盲测。按 Media 做 cluster bootstrap，不能把同一作品多集当作完全独立风险样本。

性能比较必须使用同源、同音轨、同配置，覆盖 cold、warm、单集追加、整季 refresh 和失败重试，并同时提供准确性结果。至少分解 queue wait、remote gate wait、capture、compare、refine、frame、publish；未知 input bytes 保持未知，PCM 字节不是网络流量。缓存图复用不能写成重新渲染成功。

目标、当前实现、开发验证与正式验收分别记录。完整方法和报告合同见[标记验收手册](../testing/marker-accuracy-runbook.md)与[指纹基准工具](../../scripts/fingerprint-bench/README.md)。

## 实现导航

| 职责 | 入口 |
|---|---|
| 采集与时间证明 | `crates/marker/src/fingerprint/`、`crates/marker/src/boundary/` |
| 候选、模板与 pair 缓存 | `crates/marker/src/matcher/`、`crates/marker/src/adaptive/` |
| 缓存与采样配置 | `crates/api/src/fingerprint_job/` |
| Analysis、晚 Intro 搜索与按 kind 输出 | `crates/api/src/probe_manager/analysis/` |
| 持久交接和发布 | `crates/store/src/probe_analysis/`、`crates/store/src/library/markers/` |
| 章节任务与产物身份 | `crates/store/src/chapter_frame_tasks/` |
| 章节执行与恢复 | `crates/api/src/chapter_frame_service/` |
| 章节 API | `crates/api/src/http/library_chapters.rs` |
| 准确性与性能报告 | `scripts/fingerprint-bench/` |
