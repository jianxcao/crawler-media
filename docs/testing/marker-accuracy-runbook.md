# Marker 准确性证据与本地复核

适用 `scripts/fingerprint-bench/` 的严格评价 JSON、本地复核页和生产 timing runner。设计见 [片头片尾与章节优化](../design/marker-optimization.md)。软件测试通过不等于准确率通过；没有独立人工音视频标签、裁定和冻结 holdout 时必须报告 **insufficient**。边界以真实媒体时间线和人工允许区间为准，不通过放宽容忍度或固定 offset 掩盖误差。生产耗时、准确性和相对性能收益分别举证。

## 1. 原始证据与双人盲标

1. 为完整本地源计算 SHA-256，记录来源/许可、原始时长、媒体 timeline 起点、所选音轨全局 stream index、编码、语言、FFmpeg/Chromaprint 与播放器版本。STRM 文本、URL、局部 WAV/窗口 hash 均不能冒充完整视频 hash；无法取得完整源证据时保留 unknown。
2. 每个 `(source, episode, kind)` 一个 case，intro/outro 各一行。以 Media 划分 development / validation / holdout，同一 Media 的各季、剪辑、转码、配音均留在同一分区；case_id 是稳定不透明标识，不能从文件名推断集数或身份。
3. 两名标注者独立看画面并听声音，边界前后至少 10 秒，必要时查看更长上下文；不看预测，也不看另一人的标签。每人保存自己的原始记录及时间戳；本页首次标注只加载 manifest，不加载他人 labels。页面是复核工具，不能代替原始标注保存流程。
4. 标注的是可安全跳过的音视频区间。对白、片头前正片、与音乐重叠的对白、片尾中段/末尾彩蛋均为 protected。safe 与 protected 不能交叠，不能跨彩蛋合并。没有片头/片尾明确标 absent，缺标签不是 absent。边界允许区间表达真实不确定性；有分歧且未能裁定的保留 ambiguous，不能删除难例。
5. 裁定者记录原始双人标签、分歧、裁定依据、最终允许边界与 protected 区间，保留三者的真实身份与记录；`reviewers` 至少两人，`adjudicated` 是事实声明。页面的“完成复核”仅是当前会话揭示预测的门禁，不写入或伪造这些字段。
6. 先以 60 集 / 10 Media 试标注，正式建议 600 个完整 source / 60 Media，development 20、validation 10、holdout 30 Media，每个约 10 集；这些是样本设计，不是已完成数量。另列无片头、冷开场、提前音乐、对白叠音乐、换曲、多片头、片尾长度变化、彩蛋、短集、不同配音/剪辑、坏 PTS、音轨变化、截断与网络失败的实际数量。故障批次单列可靠性，失败记录不能剔除。
7. 开发/验证完成后冻结样本清单、完整 hash、音轨、timeline、阈值、采样模式、模型/模板/政策版本、配置摘要与 acceptance_policy。冻结后才解封 holdout；看过结果再调参是回归，不再是独立盲测。不能把已有开发素材重命名为 holdout。人员/样本缺口、完成比例、0 人工标签均显式记录为 insufficient。

## 2. 严格评价文件合同

以 `scripts/fingerprint-bench/evaluate.py` 的公开评价入口为准，不增加旧字段兜底。manifest 是对象；labels、predictions 各自是数组，不能把 runner 整份结果 JSON 当 predictions。

- manifest：`corpus_version`、`evidence_kind`（synthetic / human_reviewed / public_annotation）、`cases`，可选 `acceptance_policy`。每个 case 精确字段为 `case_id, media_group, split, source_hash, duration_ms, kind`。
- labels 每行精确字段：`case_id, source_hash, state, safe_intervals_ms, protected_intervals_ms, start_range_ms, end_range_ms, reviewers, adjudicated`。state 为 present / absent / ambiguous；后两者 safe 为空、边界为 null。present / absent 必须裁定，present 两个允许区间必须分离。毫秒区间在原始时长内。
- predictions 每行精确字段：`case_id, source_hash, state, interval_ms, reason, run_id`。state 为 matched / no_match / uncertain / error；仅 matched 有连续 interval，其余为 null。
- 正式评分每个 case 都须恰一行标签和预测，源 hash 相等；不允许重复、意外 case、跨 Media 分区泄漏、越界。复核页允许未选 labels/predictions 或空数组，以便盲标/记录 insufficient；非空数组必须完整覆盖，不能把部分数据当成完整输入。页面不会自动补真值。

原始标注与裁定说明另存 sidecar，不往严格 JSON 塞额外字段。字段验证以 `scripts/fingerprint-bench/evaluate.py` 及其回归为准。

## 3. 本地页面与音轨校验

直接在浏览器打开 `scripts/fingerprint-bench/annotation-review.html`，无需服务、前端构建或生产实例。页面只加载同目录本地脚本，CSP 禁止网络请求；视频来自手选文件 object URL。文件名、case、source_hash、指定音轨声明均可见，预测内容在显式完成当前 case 复核后再按“揭示”显示。切换 case、重新加载 JSON、替换视频会重置门禁；已看过的预测不能通过刷新页面变回独立盲标。

1. 加载 manifest；独立原始标签形成后才加载 labels。可提前加载 predictions，但不要在文件编辑器中查看。选 case，核对 Media、kind、split、完整 source_hash 与原时长。
2. 手选同一完整本地源。小于等于 128 MiB 时浏览器计算完整 SHA-256（WebCrypto 非流式，限制避免多 GB 缓冲）；超过上限或 WebCrypto 不可用时，页面明确报错并提供人工外部核验路径：

   ```sh
   shasum -a 256 '/绝对路径/episode.mp4'
   ```

   将结果加 `sha256:` 前缀填入页面，核对所选文件的路径、文件名、大小与命令确实一致，再勾选外部核验声明。页面会标注 **人工外部核验声明，浏览器未计算**；粘贴 manifest 的 hash 不构成核验。错误 hash 拒绝绑定；源仍不能解码则不允许完成页面复核。JSON 文件每份限 8 MiB，时间整数限 JavaScript 安全整数范围。
3. 用 `ffprobe` 检查当前文件实际流；从 runner `sources.json` 抄录指定全局 `audio_stream_index`，不是“第几个音轨”或语言推测。浏览器不能可靠选择或证明实际选中的多音轨，页面始终明确显示实际音轨未知。只有经外部确认当前文件**仅一个正确音轨且时间线一致**才勾选声明。还应确认播放器可听到声音；静态截图不是证据。
4. 通过“标签起点/终点 ±10 秒”播放原始时间范围，允许边界区间两端都覆盖，并在范围末端自动暂停。播放器可以手动继续查看；原生 controls 未静音，按钮播放亦取消静音并设置音量。播放器错误、`play()` 拒绝、源/时长不匹配均显示并阻断复核。时长差超过 1 秒用于发现文件身份问题，不是边界容忍度；同长错源由 hash 检查。
5. 外部原始记录与裁定完成后勾选复核完成，点击确认，再单独点击揭示。可听看预测两端。signed error 距人工允许区间，提前为负、滞后为正、区间内为 0；不适用时显示 N/A。复核所有 protected overlap、FP、超差端点，再随机抽查 10% 通过样本，保留抽样种子与结果。

### 固定单音轨副本与不支持解码

优先保持原源，使用能显式选择音轨和显示精确时间的本地播放器（例如已安装的 mpv / VLC）外部复核；记录实际选择与播放器版本。浏览器不可播放时不要声称页面已完成验证，不要自动转码或移位。若需要浏览器可播放的固定单音轨评测副本，可以在授权的本地准备步骤尝试如下命令（示例选择原文件的**全局 stream 1**，先核实 index/codec；不覆盖原文件）：

```sh
ffprobe -v error -show_format -show_streams -of json '/绝对路径/original.mp4' > /tmp/original-streams.json
ffmpeg -n -copyts -i '/绝对路径/original.mp4' -map 0:v:0 -map 0:1 -c copy -avoid_negative_ts disabled '/绝对路径/fixed-audio.mp4'
ffprobe -v error -show_format -show_streams -of json '/绝对路径/fixed-audio.mp4' > /tmp/fixed-streams.json
shasum -a 256 '/绝对路径/original.mp4' '/绝对路径/fixed-audio.mp4'
```

此命令仅为保留时间戳的 remux 起点，**不保证所有容器/编码都保留播放器时间线**，也不保证浏览器支持 codec。不得用 `-ss` 裁剪、`-start_at_zero` 或 `setpts/asetpts` 偷移时间。校验两者起点、时长、视频首尾与标注边界附近帧 PTS、所选音轨首尾/边界附近的解码 sample PTS、听觉与画面同步；页面采用零起点原媒体秒数，非零/负起点或 edit list 映射无法证明一致时使用外部播放器，不能猜 offset。转码引入的 encoder delay 也必须单独验证；不确定则不能沿用旧标签。

固定副本是新的完整源：保留原源→副本 hash 和原始 stream→新 stream 映射/命令/验证记录；重新固定 manifest 的 source_hash、sources 的路径和新**全局** stream index，并对同一副本重新标注/验证及跑预测。页面拒绝拿副本冒充原 hash；不要拿原源预测与另一个副本的标签混评。同一 Media 的副本仍属同一 split。浏览器没有自动判断音轨/PTS 正确性的能力，外部声明不会被标成机器确认。

## 4. 当前生产管线耗时（首次与复用刷新）

先准备真实完整 hash、明确集数及音轨的 manifest/sources。编译使用仓库既有构建缓存，运行数据写本轮独立 `/tmp` 目录。每个 source mapping 为：

```json
[{"case_id":"opaque-intro-id","path":"/absolute/episode.mp4","audio_stream_index":1,"unit":{"season":1,"episode":1}}]
```

每个 manifest case 都需要映射，intro/outro 同文件映射一致；Media 来自 manifest，unit 显式给定。禁止复用文件名猜身份。准备与编译在计时外；正式测量独占两个构建槽，当前 mode 示例 `full_window`，使用 adaptive 时单独记录，不混合结果。

```sh
python3 scripts/with-build-lock.py --exclusive cargo build --release -j8 -p api --bin fingerprint-bench
CM_EVAL_DIR=$(mktemp -d /tmp/cm-marker-eval-XXXXXX)
# 将这两个变量设为已验证输入的绝对路径。
CM_MANIFEST=/absolute/manifest.json
CM_SOURCES=/absolute/sources.json
python3 scripts/with-build-lock.py --exclusive target/release/fingerprint-bench \
  --manifest "$CM_MANIFEST" --sources "$CM_SOURCES" \
  --mode full_window --capture-policy recapture --scenario cold \
  --data-dir "$CM_EVAL_DIR/data" --output "$CM_EVAL_DIR/runs" --run-id current-first
python3 scripts/with-build-lock.py --exclusive target/release/fingerprint-bench \
  --manifest "$CM_MANIFEST" --sources "$CM_SOURCES" \
  --mode full_window --capture-policy reuse_valid --scenario refresh \
  --data-dir "$CM_EVAL_DIR/data" --output "$CM_EVAL_DIR/runs" --run-id current-refresh
```

必须先检查首次 exit/status，失败要诊断，不能直接把随后重试称正常复用测量。`cold` 要求新导入；`warm` 要求已存在；`refresh` 明确 reuse_valid / recapture；`incremental` 需要旧文件与新到文件、仅排新到文件；`retry` 是生产手动重试。后两者仅接受 reuse_valid。没有等价输入和完整配对数据时只报告实际耗时，不宣称百分比收益。

每个 stdout Media 行和 `<run-id>.json` 的 `media[]` 都给 `independent_episode_count`、`file_count`、`elapsed_ms`、`terminal_status`、`wallclock_complete`、`frame_targets`、`ready_frames`。报告首次和复用刷新分别的实际值。计时从 enqueue 前到该 Media 的识别、探测释放、所有要求的 scene JPEG 终态且成功解码；哈希、导入、工具发现、编译和 teardown 不在 Media 时钟内。失败/缺图 elapsed 只列诊断，不能作为完整耗时；无截图目标应明确报 0，不声称执行过 JPEG。application cold 不代表 OS cache cold。阶段和可能重叠，不能求和代替墙钟。`measurement_complete` 是网络流量完整性，不等于 `wallclock_complete`；未知 input_bytes 为 null。

每次同时产出 `<run-id>-predictions.json`，它才是页面/CLI 的 predictions 输入。不得把 STRM 内容当完整远程 hash；即使 `--allow-remote`，未验证远程完整源仍产出 unverified/error。远程媒体只使用已授权来源，并协调已有读取任务，保持 REMOTE_READ_LIMIT=1 / REMOTE_READ_COOLDOWN=500ms；不启动或重启其他测试正在使用的实例。

### 按 Intro/Outro 分开判读产物

任务执行 succeeded/completed 与识别结果是两回事。检查单元的 `detection_outcome_json`：整体 `verified` 表示两种 kind 都已解决，`partial` 表示只有一种已解决，`uncertain` 表示两种都未解决。每个 `kinds.intro` / `kinds.outro` 分别保存 `decision`、`status`、`reason`、`retained_previous`：

```json
{"status":"partial","kinds":{"intro":{"decision":"verified","status":"verified","reason":null,"retained_previous":false},"outro":{"decision":"uncertain","status":"uncertain","reason":"verification_unresolved: Outro no_valid_templates","retained_previous":false}}}
```

`decision` 是本次算法原始结果；`status` 还包括缺失覆盖检查，例如原始 NoMatch 因覆盖不足会保持 `decision=no_match, status=uncertain`。Verified 可独立保存对应 kind 并生成当前章节图；另一个 Uncertain 保留同一当前源版本已有值，不借旧源结果。NoMatch 必须满足严格 absence coverage 才能清除旧区间。全部 Uncertain 不重写标记/章节。刷新仍是一个有完整 source/claim/Library/root/coverage 校验的事务，识别不确定不再冒充网络或计算失败。

人工 locked 区间或旧当前版本区间若被保留，`retained_previous=true`。即使算法 status=verified，也不能把实际未采用的新区间、已有人工标记或旧截图算成本次自动生成。benchmark 按 case 的 kind 判读，不允许已确认 Intro 掩盖未确认 Outro。图必须逐目标检查 current generation/attempt、ready/published 和实际解码；片头图与正片章节图分别计数，不能把所有章节图都叫片头图。

因此 `wallclock_complete=true` 只说明本次处理及其已产生的帧义务到达完整终态，**不证明所有预期 case 都识别成功**。报告必须同时列出每 kind 的 matched/uncertain/error、实际写入文件数、当前解码图片数与保留旧结果数。零目标明确为零；部分 kind 成功只能报告该 kind 的真实产物和时间，不能称整部 Media 的片头片尾均成功。使用已有图的复查时间也不等于重新生成图片的时间。

### 自适应模式下的晚片头

`adaptive` 的初始搜索仍按默认前 180 秒；证据合格且片头未确认时，有限补采前部重叠窗口，默认发现序列为 `[0,180]`、`[120,420]`、`[300,600]` 秒，每次独立窗口不超过 300 秒，受真实 duration 夹取。`full_window` 仍用其原配置窗口，不自动切换模式。

每 kind 的窗口预算最多 3、物理采集 attempt 最多 4，包含已用的预测窄窗、回退、失败及中断，不是额外赠送三个窗口；deadline 不增大。缓存复用不算物理采集，新读取必须由持久预算准入。坏时间轴、实际缺音频、源事实不符或网络错误不能靠扩窗伪装成有效结果。未知时长保持未知，不发明片尾位置；补采上界不表示视频确实存在到 600 秒。

不同集的同一主题可以处于不同绝对位置，例如一集 330..410 秒，另两集 30..110 秒。参考仍按独立 episode 计数，每份 capture 保留自己的真实 origin 和 sample identity；禁止拼接独立读取的 PCM 伪造连续采样。已确认片头优先用模型预测后续集的窄窗，不因其他集或片尾未确认反复读取。

该有限序列不保证每个最长 240 秒片头都完整落入单窗。跨窗且缺完整上下文、冲突候选和预算用尽必须显式 SamplingLimit/Uncertain；局部 NoMatch 不能证明整个 0..600 秒没有片头，不能借此清除已有晚片头。统计这些未解决 case，不从评测分母删除。损坏或不支持的持久采样计划应有限失败，不能无期限重试。

## 5. 评分命令与证据判读

`report_cli.py --results` 会读取目录内**所有 JSON** 并要求它们都是 run 对象；runner 输出目录还含 predictions 数组，不能直接传它。只复制需要评分的 run 对象到独立目录，例如评分本次首次运行：

```sh
mkdir "$CM_EVAL_DIR/report-runs"
cp "$CM_EVAL_DIR/runs/current-first.json" "$CM_EVAL_DIR/report-runs/"
CM_LABELS=/absolute/independently-adjudicated-labels.json
python3 scripts/with-build-lock.py python3 scripts/fingerprint-bench/report_cli.py \
  --results "$CM_EVAL_DIR/report-runs" --manifest "$CM_MANIFEST" --labels "$CM_LABELS" \
  --predictions "$CM_EVAL_DIR/runs/current-first-predictions.json" \
  --output "$CM_EVAL_DIR/accuracy.md"
```

没有标签时显式提供 `[]` 文件并报告 insufficient，不制造 reviewers 或从预测复制 truth。CLI 成功通过 exit 0，完整但失败 exit 1，invalid/insufficient exit 2；生成报告不意味着通过。软件合同错误为 invalid；缺标签/人数/冻结政策/代表性样本为 insufficient；完整证据未达标为 failed。synthetic 只能验证工程契约，不能通过真实准确性门槛。

intro/outro 分开报告 TP/FP/FN、precision/recall、正例与预测分母、覆盖率、ambiguous/error/uncertain、signed/absolute 起止偏差 p50/p95、两端同时 ±1s / ±2s、任意与 >2s protected overlap，另列 Media macro 与 cluster bootstrap 区间。宽松 IoU≥0.5 仅分类检测 TP，不能宣称边界准确。30 个独立 Media 全零失败的单侧 95% 上界仍为约 9.503%；300 个独立单位约 0.994%。300 集来自 30 Media 不能冒充 300 个独立组。

保存代码 commit、工具版本、配置摘要、输入三份 JSON、源/音轨/timeline sidecar、原始双人标注、裁定理由、冻结记录、原始阶段耗时、拒绝/失败、截图终态与报告。临时证据放本次 mktemp 目录，交付后仅清理明确归属本次的目录；不删除已有样本或共享构建缓存，不提交视频、凭证、`docs/test-reports/`、`.e2e/`。shadow 仅记录候选，不自动改默认模式；扩大启用范围需要独立决策。
