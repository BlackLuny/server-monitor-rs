//! Random token generation for `join_token` and `server_token`.
//!
//! We use 32 bytes of OS randomness encoded as URL-safe base64 without padding,
//! giving a ~43 character opaque string that is safe to paste into shell
//! commands and URLs.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::RngCore;

/// Length of the random secret in bytes.
pub const TOKEN_BYTES: usize = 32;

/// Generate a fresh 32-byte random token, encoded as url-safe base64 (no pad).
#[must_use]
pub fn generate() -> String {
    let mut buf = [0u8; TOKEN_BYTES];
    rand::thread_rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_non_empty_and_unique() {
        let a = generate();
        let b = generate();
        assert!(!a.is_empty());
        assert_ne!(a, b);
        // URL-safe base64 of 32 bytes without padding = 43 chars.
        assert_eq!(a.len(), 43);
    }
}
