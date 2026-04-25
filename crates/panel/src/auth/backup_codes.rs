//! One-time backup codes for TOTP.
//!
//! Each code is 8 uppercase alphanumerics formatted as two dash-separated
//! quartets (e.g. `A7K2-4F9M`). That format is short enough to type under
//! pressure and just long enough to make brute-force irrelevant (32 bits of
//! unambiguous characters, rate-limited by the argon2 verify cost).
//!
//! Storage: `users.backup_codes` is a JSONB array of argon2 hashes. When a
//! code is consumed at login we rewrite the column without that hash — this
//! matches the "one-time" contract and means a DB dump is worthless.

use rand::Rng;
use serde_json::Value;

use super::password;

/// Characters sampled for each code character. Omits 0/O/1/I/L to avoid the
/// ambiguity rows of characters that look alike in monospaced fonts.
const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const GROUPS: usize = 2;
const GROUP_LEN: usize = 4;
pub const CODE_LEN: usize = GROUPS * GROUP_LEN + GROUPS - 1;
pub const CODE_COUNT: usize = 10;

/// Generate `CODE_COUNT` fresh codes in plaintext. Callers hash before
/// persisting (see [`hash_all`]) and show them to the user exactly once.
#[must_use]
pub fn generate_plaintext() -> Vec<String> {
    let mut rng = rand::thread_rng();
    (0..CODE_COUNT).map(|_| one(&mut rng)).collect()
}

/// Hash every code with argon2 — same parameters the password system uses.
/// Errors surface as the first hashing failure.
pub fn hash_all(plain: &[String]) -> Result<Vec<String>, password::PasswordError> {
    plain.iter().map(|c| password::hash(c)).collect()
}

/// Try to consume `candidate` against the JSONB array in `stored`. On success
/// returns the array with the matched hash removed — callers persist that
/// back to the row. On mismatch returns `None`.
#[must_use]
pub fn consume(stored: &Value, candidate: &str) -> Option<Value> {
    let arr = stored.as_array()?;
    let normalized = candidate.trim().to_ascii_uppercase();
    for (idx, entry) in arr.iter().enumerate() {
        let hash = match entry.as_str() {
            Some(s) => s,
            None => continue,
        };
        if password::verify(&normalized, hash).unwrap_or(false) {
            let mut remaining = arr.clone();
            remaining.remove(idx);
            return Some(Value::Array(remaining));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// internals
// ---------------------------------------------------------------------------

fn one<R: Rng>(rng: &mut R) -> String {
    let mut out = String::with_capacity(CODE_LEN);
    for g in 0..GROUPS {
        if g > 0 {
            out.push('-');
        }
        for _ in 0..GROUP_LEN {
            let ch = ALPHABET[rng.gen_range(0..ALPHABET.len())];
            out.push(ch as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn codes_have_expected_shape() {
        let codes = generate_plaintext();
        assert_eq!(codes.len(), CODE_COUNT);
        for c in &codes {
            assert_eq!(c.len(), CODE_LEN);
            assert_eq!(c.chars().nth(GROUP_LEN), Some('-'));
            for ch in c.chars() {
                assert!(ch == '-' || ALPHABET.contains(&(ch as u8)));
            }
        }
    }

    #[test]
    fn consume_matches_once() {
        let codes = generate_plaintext();
        let hashes = hash_all(&codes).unwrap();
        let stored = Value::Array(hashes.into_iter().map(Value::String).collect());

        let pick = &codes[3];
        let after = consume(&stored, pick).expect("should consume");
        assert_eq!(after.as_array().unwrap().len(), CODE_COUNT - 1);

        // Second use with same code must fail against the trimmed array.
        assert!(consume(&after, pick).is_none());
    }

    #[test]
    fn consume_ignores_case_and_whitespace() {
        let codes = generate_plaintext();
        let hashes = hash_all(&codes).unwrap();
        let stored = Value::Array(hashes.into_iter().map(Value::String).collect());

        let pick = format!("  {}  ", codes[0].to_ascii_lowercase());
        assert!(consume(&stored, &pick).is_some());
    }

    #[test]
    fn consume_rejects_garbage() {
        let stored = json!([]);
        assert!(consume(&stored, "ZZZZ-ZZZZ").is_none());
    }
}
