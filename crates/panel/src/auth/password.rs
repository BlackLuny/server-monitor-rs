//! Argon2id password hashing.
//!
//! The same primitive is also used for backup codes in `session::backup_codes`.
//! `hash` takes a plaintext and produces a PHC-encoded string ready to store
//! verbatim in a `TEXT` column; `verify` tolerates any hash `argon2` supports,
//! which lets us rotate parameters without a migration.

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};

#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    #[error("argon2 hashing failed: {0}")]
    Hash(argon2::password_hash::Error),
    #[error("stored hash is malformed")]
    MalformedHash,
}

/// Hash `plaintext` with Argon2id and a fresh OS-random salt.
pub fn hash(plaintext: &str) -> Result<String, PasswordError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(plaintext.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(PasswordError::Hash)
}

/// Verify `plaintext` against a previously produced `hash`. Returns `Ok(true)`
/// on match, `Ok(false)` on mismatch, and `Err` only when the stored hash is
/// itself unparseable (which would be a bug or corruption).
pub fn verify(plaintext: &str, stored_hash: &str) -> Result<bool, PasswordError> {
    let parsed = PasswordHash::new(stored_hash).map_err(|_| PasswordError::MalformedHash)?;
    match Argon2::default().verify_password(plaintext.as_bytes(), &parsed) {
        Ok(()) => Ok(true),
        Err(argon2::password_hash::Error::Password) => Ok(false),
        Err(e) => Err(PasswordError::Hash(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let h = hash("hunter2").unwrap();
        assert!(verify("hunter2", &h).unwrap());
        assert!(!verify("wrong", &h).unwrap());
    }

    #[test]
    fn malformed_hash_is_error() {
        assert!(matches!(
            verify("x", "not-a-hash"),
            Err(PasswordError::MalformedHash)
        ));
    }
}
