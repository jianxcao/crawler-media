use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};

pub fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    argon2
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| e.to_string())
}

/// Verify a password against a stored credential.
///
/// Storage only holds Argon2id PHC strings. A row in any other shape is a
/// pre-cutover leftover: it fails closed instead of being compared verbatim,
/// so a stale plaintext row can neither log in nor be silently upgraded.
pub fn verify_password(password: &str, stored: &str) -> bool {
    let Ok(parsed_hash) = PasswordHash::new(stored) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .is_ok()
}

pub fn is_argon2_hash(stored: &str) -> bool {
    stored.starts_with("$argon2")
}

/// 该持久凭据是否处于登录路径唯一接受的可校验哈希形态。
///
/// PHC 语法合法不代表存在可验证的密码：还需要 Argon2 算法、有效参数、盐和输出。
pub fn is_verifiable_hash(stored: &str) -> bool {
    let Ok(hash) = PasswordHash::new(stored) else {
        return false;
    };
    hash.salt.is_some()
        && hash.hash.is_some()
        && argon2::Algorithm::try_from(hash.algorithm).is_ok()
        && hash
            .version
            .is_none_or(|version| argon2::Version::try_from(version).is_ok())
        && argon2::Params::try_from(&hash).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_and_verify_argon2() {
        let password = "my-secret-password-123";
        let hash = hash_password(password).expect("hashing should succeed");
        assert!(is_argon2_hash(&hash));
        assert!(verify_password(password, &hash));
        assert!(!verify_password("wrong-password", &hash));
    }

    #[test]
    fn test_plaintext_row_fails_closed() {
        let plaintext = "legacy-plain-pwd";
        assert!(!is_argon2_hash(plaintext));
        assert!(!verify_password(plaintext, plaintext));
        assert!(!verify_password("wrong-password", plaintext));
    }

    #[test]
    fn test_malformed_argon2_row_fails_closed() {
        assert!(!verify_password("secret", "$argon2id$not-a-phc-string"));
    }
}
