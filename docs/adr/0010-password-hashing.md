# ADR-0010: 用户密码采用 Argon2id 哈希存储

> 本文保留密码哈希与当时明文升级决策。**密码行的历史处理不等于旧数据库布局迁移**：当前 Store 仅接受当前 `application_id` / `user_version` 或空文件，没有旧布局导入器（`crates/store/src/schema/layout.rs`）。不得按本文对旧实例执行原地数据库升级。

## 背景

ADR-0003 记录了 PT 站点凭证（Cookie/API key/Passkey）以明文存储在 SQLite 中，因为私有 NAS 威胁模型以及防丢密钥。
然而，自托管媒体系统的登录账号（Admin 及各 Member 用户）密码属于用户个人敏感身份凭据，用户常在多系统间复用口令。如果以明文存储在 `users.password` 中，一旦数据目录备份泄露或数据库被审计读取，将造成严重凭据安全隐患。

## 决策

1. **强哈希算法**：用户密码采用业界推荐的 **Argon2id** 算法进行哈希存储（包含独立随机 Salt，防彩虹表与 GPU/ASIC 暴破）。
2. **凭据与站点隔离**：本决策仅适用于 `users` 表的用户登录密码，不推翻 ADR-0003 中针对 PT 站点的明文凭证约定。
3. **历史明文密码处理与当前残留实现**：
   - `crates/store/src/password.rs` 当前以 `$argon2` 前缀识别哈希，解析/校验失败即失败；其他字符串仍走明文比较。不能笼统称其支持任意 PHC 算法。
   - `crates/api/src/password_login.rs` 仍有已接受布局内明文密码成功登录后重哈希路径。这是现存历史处理，不是建议继续保留兼容层，也不能让旧 schema 通过布局预检。
   - 所有新建用户与修改密码操作（`POST /api/v1/users`、`PATCH /api/v1/users/{id}`、种子默认管理员初始化）直接生成 Argon2id 哈希存储。
4. **种子管理员密码与 CLI Bearer Token 分离**：
   - 初始管理员密码（`CRAWLER_MEDIA_ADMIN_PASSWORD`）仅用于密码哈希计算并存入 `users.password`，绝对不作为明文会话写入 `user_tokens` 表；
   - 独立配置 `CRAWLER_MEDIA_TOKEN` 用于 CLI 或受信服务调用；正常已有管理员不会每次重启被环境变量改密。当前 bootstrap 仍检测已接受布局内口令等于已知 CLI token 的不安全状态，并要求生产启动提供独立密码后轮换（`crates/api/src/bootstrap_credentials.rs`）；这不是旧库升级承诺。
   - **历史备份安全边界**：此前版本备份中 `users.password` / `user_tokens` 可能包含明文凭证；即使新写入已使用哈希，旧备份仍敏感，须按部署方安全策略保护与轮换，不能视为已经被应用清理。
