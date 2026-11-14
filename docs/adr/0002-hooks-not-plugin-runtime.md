# 进程内 Hook 总线；第一方插件编译期内置；第三方加载器以后再说

工作流步骤（选择 Torrent、加入 Downloader、Transfer、重命名、Scrape、站点登录、Check-in）暴露 **Hook**。核心的 Indexer / Downloader / Media / Library 是编译进二进制的模块。登录与 **Check-in** 是编译进同一二进制的第一方 **Plugin**，只通过 **Hook** 和 **Browser** 交互。用户自供插件加载器另行设计，优先考虑 WASM；当前不做 `dlopen` 或内嵌脚本运行时。

产品需要工作流干预点和可选站点维护。让第一方插件按同一契约编译，保持单一扩展模型，避免在登录/Check-in 形态定型之前冻结加载器 ABI。

**备选方案**：WASM；Rhai/Lua；`dlopen`；把登录/Check-in 做成核心 Indexer 代码。
