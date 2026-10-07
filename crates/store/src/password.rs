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

pub fn verify_password(password: &str, stored: &str) -> bool {
    if is_argon2_hash(stored) {
        if let Ok(parsed_hash) = PasswordHash::new(stored) {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed_hash)
                .is_ok()
        } else {
            false
        }
    } else {
        // 向前兼容存量明文密码
        password == stored
    }
}

pub fn is_argon2_hash(stored: &str) -> bool {
    stored.starts_with("$argon2")
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
    fn test_legacy_plaintext_compatibility() {
        let plaintext = "legacy-plain-pwd";
        assert!(!is_argon2_hash(plaintext));
        assert!(verify_password(plaintext, plaintext));
        assert!(!verify_password("wrong-password", plaintext));
    }
}
