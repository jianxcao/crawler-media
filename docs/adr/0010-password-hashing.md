# ADR-0010: 用户密码采用 Argon2id 哈希存储

## 背景

ADR-0003 记录了 PT 站点凭证（Cookie/API key/Passkey）以明文存储在 SQLite 中，因为私有 NAS 威胁模型以及防丢密钥。
然而，自托管媒体系统的登录账号（Admin 及各 Member 用户）密码属于用户个人敏感身份凭据，用户常在多系统间复用口令。如果以明文存储在 `users.password` 中，一旦数据目录备份泄露或数据库被审计读取，将造成严重凭据安全隐患。

## 决策

1. **强哈希算法**：用户密码采用业界推荐的 **Argon2id** 算法进行哈希存储（包含独立随机 Salt，防彩虹表与 GPU/ASIC 暴破）。
2. **凭据与站点隔离**：本决策仅适用于 `users` 表的用户登录密码，不推翻 ADR-0003 中针对 PT 站点的明文凭证约定。
3. **凭据验证**：
   - `crates/store/src/password.rs` 通过 Argon2 验证 PHC 串；不可解析、参数无效或密码不匹配时拒绝登录，不做原文比较。
   - 所有新建用户与修改密码操作（`POST /api/v1/users`、`PATCH /api/v1/users/{id}`、种子默认管理员初始化）直接生成 Argon2id 哈希存储。
4. **种子管理员密码与 CLI Bearer Token 分离**：
   - 初始管理员密码（`CRAWLER_MEDIA_ADMIN_PASSWORD`）仅用于密码哈希计算并存入 `users.password`，绝对不作为明文会话写入 `user_tokens` 表；
   - 独立配置 `CRAWLER_MEDIA_TOKEN` 用于 CLI 或受信服务调用；首次管理员初始化需要独立密码，不得与当前或已知 CLI Bearer 凭据相同。
   - 已有管理员保留其持久凭据，重启不使用环境变量覆盖密码。`admin_password_is_set` 对非空无效哈希报错，启动不自动修复凭据。实现见 `crates/store/src/users.rs` 和 `crates/api/src/bootstrap_credentials.rs`。
   - 数据库及备份仍包含可用的 Site 凭证与会话 token，须按部署方的秘密数据策略保护。
