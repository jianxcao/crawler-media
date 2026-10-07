# 进程内 Hook 总线；第一方插件编译期内置；第三方加载器以后再说

工作流步骤（选择 Torrent、加入 Downloader、Transfer、重命名、Scrape、站点登录、Check-in）暴露 **Hook**。核心的 Indexer / Downloader / Media / Library 是编译进二进制的模块。登录与 **Check-in** 是第一方 **Plugin**：v1 中仍编译进同一二进制，但它们只通过 **Hook** 和 **Browser** 交互，因此以后可以移到加载器之后。用户自供插件仍是未来的加载器（优先 WASM）。v1 不做 `dlopen`，不做内嵌脚本运行时。

产品需要的是干预点加 MoviePilot 式的可选站点维护，而不是插件市场。让第一方插件按同一契约编译，保持单一扩展模型。v1 就交付 WASM 会在登录/Check-in 形态定型之前冻结 ABI。

**备选方案**：v1 用 WASM；Rhai/Lua；`dlopen`；把登录/Check-in 做成核心 Indexer 代码。
