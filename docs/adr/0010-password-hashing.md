# ADR-0010: 用户密码采用 Argon2id 哈希存储与平滑迁移

## 背景

ADR-0003 记录了 PT 站点凭证（Cookie/API key/Passkey）以明文存储在 SQLite 中，因为私有 NAS 威胁模型以及防丢密钥。
然而，自托管媒体系统的登录账号（Admin 及各 Member 用户）密码属于用户个人敏感身份凭据，用户常在多系统间复用口令。如果以明文存储在 `users.password` 中，一旦数据目录备份泄露或数据库被审计读取，将造成严重凭据安全隐患。

## 决策

1. **强哈希算法**：用户密码采用业界推荐的 **Argon2id** 算法进行哈希存储（包含独立随机 Salt，防彩虹表与 GPU/ASIC 暴破）。
2. **凭据与站点隔离**：本决策仅适用于 `users` 表的用户登录密码，不推翻 ADR-0003 中针对 PT 站点的明文凭证约定。
3. **平滑升级与向前兼容**：
   - 存储格式：如果存储的字符串以 `$argon2id$`（或标准 PHC 格式）开头，按 Argon2id 进行校验；如果不是，则按存量明文比较。
   - 透明升级：当用户使用存量明文密码成功登录时，系统在验证通过后自动将该用户的密码原地重新哈希为 Argon2id 格式更新回数据库。
   - 所有新建用户与修改密码操作（`POST /api/v1/users`、`PATCH /api/v1/users/{id}`、种子默认管理员初始化）直接生成 Argon2id 哈希存储。
4. **种子管理员密码与 CLI Bearer Token 分离**：
   - 初始管理员密码（`CRAWLER_MEDIA_ADMIN_PASSWORD`）仅用于密码哈希计算并存入 `users.password`，绝对不作为明文会话写入 `user_tokens` 表；
   - 独立配置 `CRAWLER_MEDIA_TOKEN` 用于 CLI 或系统服务间免交互的 Bearer Token，二者职责与生命周期完全解耦。对于旧实例，若现有密码仍等于 CLI token，配置独立密码后将自动轮换密码并清理掉历史明文残留。
   - **历史备份与安全轮换建议**：在升级前产生的历史数据库备份中，`users.password` 与 `user_tokens` 可能仍包含旧版本的明文 token。操作者在升级完成后，应按安全规范主动轮换生产环境中的 `CRAWLER_MEDIA_TOKEN` 并妥善清理或重新加密历史备份文件。
