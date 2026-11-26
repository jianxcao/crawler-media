# 远程媒体信息与跳转解析

媒体信息探测读取文件的编码、分辨率、HDR、音轨、字幕和时长；TMDB 元数据识别与 NFO/图片刮削是另一流程。两者完成状态独立，缺少某集图片时的截图回退会与媒体信息、声纹共享远程读取准入。

## 默认探测预算

`marker::ProbeTarget` 为远程 ffprobe 配置 `probesize=10000000` 字节、`analyzeduration=10000000` 微秒媒体时间，以及 30 秒网络 IO 超时。前两者是探测预算，并非每次必然读满的数量，也不是整个 HTTP 响应体接收量的硬上限。容器头、尾部索引、跳转与缓存会影响请求数量和字节数。TheIntroDB 是 JSON 查询，本身不读取这些视频字节。

保守保留 10MB / 10s 默认预算，以覆盖延迟音轨、稀疏字幕、不同容器头尾索引及远程 Range 行为；盲目缩小预算容易漏检后置流而未必降低首包延迟。后续调整须使用隔离的 MKV/MP4/TS、字幕及多音轨样本，比较实际接收字节、请求数、完整字段、p50/p95 和错误率；不得把配置值直接写成实测流量。

## 302 预解析

第一次对输入地址发送 `GET Range: bytes=0-0`，禁用自动跳转，只取得 Location 后将目标交给 ffprobe/ffmpeg。该 Range 仅用于解析入口，不是后续视频读取范围；真实 302 本来也需要入口请求，缓存可避免 seek 重复访问跳转服务。缓存上限 4096 项，默认最长 TTL 为 300 秒，并按响应 `Cache-Control`（`no-store`/`no-cache` 不缓存，`s-maxage`/`max-age` 取更短值）、`Expires` 以及签名直链过期参数（`Expires`/`exp`/`expire`、`X-Amz-Date`+`X-Amz-Expires` 预留 5 秒安全余量）向下夹取；直接返回成功 200/206 的输入也缓存为自身，避免无跳转源每次重复预检。HTTP/传输失败不缓存成成功结果，仍回退原输入，且记录失败类别。

缓存不读取视频正文；首次解析仍有独立客户端、连接与最长 10 秒解析超时的成本，不能仅凭机制宣称更快。日志 `STRM target resolution completed` 提供 `cache_hit`、`redirected`、`elapsed_ms`，不输出带凭据的完整 URL。与纯 ffprobe 的对照须包含首次、缓存命中、直接源、解析失败和多次 seek。

## 耗时边界

持久队列的 `queue_wait_ms` 与 worker 里的媒体信息耗时分别计算。`Remote media read admitted.waited_ms` 是共享闸门等待；单文件「远程准入与流信息读取完成」日志的 `elapsed_ms` 包含闸门与实际 probe，任务汇总日志的 `media_info_elapsed_ms` 累计媒体信息阶段总耗时；NFO 日志中的 `nfo_elapsed_ms` 单独记录 NFO 发布耗时（同行的 `elapsed_ms` 从媒体信息阶段起点累计，不能解释为单次 NFO 写入耗时）；最后数据库发布仍由源版本 CAS 保护。

现有日志仍不提供每次 HTTP 响应体精确字节数，也不能拆分 DNS、TLS、CDN 首字节和解析 CPU。系统网络采样包含协议开销且可能漏采，不替代应用字节计数。共享准入的界面配置、默认值及热修改行为见[探测设置](../api-contracts/probe-settings.md)。
