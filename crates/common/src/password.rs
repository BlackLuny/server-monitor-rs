//! Password hashing / verification using Argon2id with sensible defaults.

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHasher, PasswordVerifier, SaltString},
    Argon2, PasswordHash,
};

use crate::error::{Error, Result};

/// Hash a plaintext password with Argon2id using a freshly-generated salt.
///
/// Returns the PHC-formatted string (safe to store in Postgres).
pub fn hash(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon = Argon2::default();
    argon
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| Error::PasswordHash(e.to_string()))
}

/// Verify a plaintext password against a stored PHC hash string.
///
/// Returns `Ok(true)` on match, `Ok(false)` on mismatch, and `Err` only when the
/// stored hash itself could not be parsed (indicating corruption).
pub fn verify(password: &str, stored: &str) -> Result<bool> {
    let parsed = PasswordHash::new(stored).map_err(|e| Error::PasswordHash(e.to_string()))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let hashed = hash("hunter2").unwrap();
        assert!(verify("hunter2", &hashed).unwrap());
        assert!(!verify("wrong", &hashed).unwrap());
    }
}
