use super::LoginCredential;
use crate::{Store, StoreError};
use domain::{User, UserId};

impl Store {
    pub fn insert_user(&self, user: &User) -> Result<(), StoreError> {
        self.with_app(|app| app.insert_user(user))
    }

    pub fn insert_user_with_prepared_password(
        &self,
        user: &User,
        prepared: &crate::password::PreparedPassword,
    ) -> Result<(), StoreError> {
        self.with_app(|app| app.insert_user_with_prepared_password(user, prepared))
    }

    pub fn update_user_with_prepared_password(
        &self,
        user: &User,
        prepared: &crate::password::PreparedPassword,
    ) -> Result<(), StoreError> {
        self.with_app(|app| app.update_user_with_prepared_password(user, prepared))
    }

    /// "admin" | "member" — defaults to member for rows without a role.
    pub fn user_role(&self, id: UserId) -> Result<String, StoreError> {
        self.role_lookups
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.with_app_read(|app| app.user_role(id))
    }

    pub fn get_user(&self, id: UserId) -> Result<Option<User>, StoreError> {
        self.with_app_read(|app| app.get_user(id))
    }

    pub fn list_users(&self) -> Result<Vec<User>, StoreError> {
        self.with_app_read(|app| app.list_users())
    }

    /// 强制设置用户角色为管理员（种子初始化使用）。
    pub fn force_admin_role(&self, user_id: UserId) -> Result<(), StoreError> {
        self.with_app(|app| app.force_admin_role(user_id))
    }

    pub fn set_user_token(&self, user_id: UserId, token: &str) -> Result<(), StoreError> {
        self.with_app(|app| app.set_user_token(user_id, token))
    }

    /// 登记当前配置的 CLI Bearer Token，并在同一事务中撤销被替换的旧 CLI token。
    /// 保证系统只有一个当前有效配置的 CLI 凭据，轮换后旧值立即失效。
    pub fn register_current_cli_token(
        &self,
        admin_id: UserId,
        token: &str,
    ) -> Result<(), StoreError> {
        self.with_app(|app| app.register_current_cli_token(admin_id, token))
    }

    /// 检查管理员是否已经设置过非空密码。用于启动自检防止空口令被误判为安全管理员。
    pub fn admin_password_is_set(&self, admin_id: UserId) -> Result<bool, StoreError> {
        self.with_app_read(|app| app.admin_password_is_set(admin_id))
    }

    /// 检查拟设置的管理员密码是否与任何已知作为系统 CLI Bearer 的密钥重合。
    /// 防止运维在初始化或迁移时将历史 CLI secret 重新用作管理员登录口令。
    pub fn admin_password_matches_known_bearer(
        &self,
        admin_id: UserId,
        proposed: &str,
        current_cli: &str,
    ) -> Result<bool, StoreError> {
        self.with_app_read(|app| {
            app.admin_password_matches_known_bearer(admin_id, proposed, current_cli)
        })
    }

    /// 在单一事务中原子撤销传入的会话 Token 列表（系统当前配置的 CLI token 不予物理删除）。
    /// 任何一个删除失败则整体回滚，绝不残留部分撤销的不一致状态。
    pub fn delete_session_tokens(&self, tokens: &[String]) -> Result<usize, StoreError> {
        self.with_app(|app| app.delete_session_tokens(tokens))
    }

    pub fn save_user(&self, user: &User) -> Result<(), StoreError> {
        self.with_app(|app| app.save_user(user))
    }

    pub fn admin_user_id(&self) -> Result<UserId, StoreError> {
        self.with_app_read(|app| app.admin_user_id())
    }

    pub fn revoke_user_tokens(&self, user_id: UserId) -> Result<(), StoreError> {
        self.with_app(|app| app.revoke_user_tokens(user_id))
    }

    pub fn finish_user_removal(&self, user_id: UserId) -> Result<bool, StoreError> {
        self.with_app(|app| app.finish_user_removal(user_id))
    }

    pub fn list_setting_keys_with_prefix(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        self.with_app_read(|app| app.list_setting_keys_with_prefix(prefix))
    }

    /// 只解析未过期的会话 token（签发后 Session TTL 秒内有效；管理员配置的系统 CLI token 持续有效）。
    pub fn user_by_token(&self, token: &str) -> Result<Option<User>, StoreError> {
        self.with_app_read(|app| app.user_by_token(token))
    }

    pub fn user_id_by_token(&self, token: &str) -> Result<Option<UserId>, StoreError> {
        self.with_app_read(|app| app.user_id_by_token(token))
    }

    /// 获取用户的最新有效会话 token（用于播放/流式 URL 凭证补全）。
    pub fn latest_token_for_user(&self, user_id: UserId) -> Result<Option<String>, StoreError> {
        self.with_app_read(|app| app.latest_token_for_user(user_id))
    }

    /// 读取密码校验所需的不可变登录快照（用户 id + 当时观察到的 password 列）。
    /// 校验本身必须在 Store 锁之外执行；之后用 [`Store::finish_verified_login`]
    /// 以 CAS 语义完成签发。`password` 为 NULL 或用户不存在/被禁用时返回 `None`。
    ///
    /// 故意不 derive `Debug`/`Serialize`：快照携带口令材料，不得进入日志。
    pub fn login_credential(&self, login: &str) -> Result<Option<LoginCredential>, StoreError> {
        self.with_app_read(|app| app.login_credential(login))
    }

    /// 在同一个事务中重查登录凭据并原子签发会话 token。
    /// 校验必须在 Store 锁之外完成；这里只接受 Argon2 PHC 观测值（明文等旧行
    /// 一律拒绝签发），若用户口令/启用状态在校验期间变更，返回 Ok(None) 并回滚，
    /// 绝不签发过期凭据。
    pub fn finish_verified_login(
        &self,
        user_id: &str,
        observed_password: &str,
    ) -> Result<Option<String>, StoreError> {
        self.with_app(|app| app.finish_verified_login(user_id, observed_password))
    }
}
