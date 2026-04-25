//! RFC-6238 TOTP helpers.
//!
//! We wrap the `totp-rs` crate with a small surface that matches the way the
//! panel stores state: the secret is the single source of truth in
//! `users.totp_secret` (base32, per the RFC), and `totp_enabled` flips only
//! after the user proves they have the generator configured.
//!
//! Verification accepts codes from the previous, current, and next 30-second
//! window. That's the usual compromise between clock drift tolerance and
//! replay resistance.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use qrcode::{render::svg, QrCode};
use rand::RngCore;
use totp_rs::{Algorithm, Secret, TOTP};

/// 20 bytes = 160 bits of entropy. Matches what Authenticator apps expect
/// and keeps the base32 representation to 32 characters.
const SECRET_BYTES: usize = 20;

/// Fixed label shown inside the TOTP app alongside the user's name.
const ISSUER: &str = "server-monitor";

/// Generate a fresh base32-encoded secret suitable for storage.
#[must_use]
pub fn new_secret() -> String {
    let mut buf = [0u8; SECRET_BYTES];
    rand::thread_rng().fill_bytes(&mut buf);
    Secret::Raw(buf.to_vec()).to_encoded().to_string()
}

/// Verify `code` against `secret` at the *current* wall-clock time.
/// Returns `false` for malformed secrets or codes.
#[must_use]
pub fn verify(secret: &str, code: &str) -> bool {
    match build(secret) {
        Ok(totp) => totp.check_current(code).unwrap_or(false),
        Err(_) => false,
    }
}

/// Build the otpauth:// URL for the authenticator's QR code. Username lands
/// as the account label so the user can tell apart multiple instances.
pub fn provisioning_url(secret: &str, username: &str) -> Result<String, totp_rs::TotpUrlError> {
    let totp = build_with_account(secret, username)?;
    Ok(totp.get_url())
}

/// Render an SVG QR code for the provisioning URL. SVG keeps the response
/// small and the UI sharp at any density — rasterizing on the backend just
/// to ship a PNG would be wasteful.
pub fn provisioning_qr_svg(url: &str) -> String {
    let code = match QrCode::new(url.as_bytes()) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    code.render::<svg::Color<'_>>()
        .dark_color(svg::Color("#e9eef6"))
        .light_color(svg::Color("transparent"))
        .quiet_zone(true)
        .min_dimensions(224, 224)
        .build()
}

/// Base64-encoded data-URL friendly wrapping of the SVG so the frontend can
/// just do `<img src={qr_data_url}>` without an extra fetch.
pub fn qr_data_url(svg: &str) -> String {
    format!(
        "data:image/svg+xml;base64,{}",
        STANDARD.encode(svg.as_bytes())
    )
}

// ---------------------------------------------------------------------------
// internals
// ---------------------------------------------------------------------------

fn build(secret: &str) -> Result<TOTP, totp_rs::TotpUrlError> {
    let bytes = Secret::Encoded(secret.to_owned())
        .to_bytes()
        .map_err(|_| totp_rs::TotpUrlError::Secret(String::new()))?;
    // Account label is irrelevant for verification.
    TOTP::new(Algorithm::SHA1, 6, 1, 30, bytes, None, String::new())
}

fn build_with_account(secret: &str, account: &str) -> Result<TOTP, totp_rs::TotpUrlError> {
    let bytes = Secret::Encoded(secret.to_owned())
        .to_bytes()
        .map_err(|_| totp_rs::TotpUrlError::Secret(String::new()))?;
    TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        bytes,
        Some(ISSUER.to_owned()),
        account.to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_is_base32_and_32_chars() {
        let s = new_secret();
        assert_eq!(s.len(), 32);
        assert!(s.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn verify_accepts_current_code() {
        let s = new_secret();
        let totp = build(&s).unwrap();
        let code = totp.generate_current().unwrap();
        assert!(verify(&s, &code));
        assert!(!verify(&s, "000000"));
    }

    #[test]
    fn provisioning_url_contains_issuer_and_account() {
        let s = new_secret();
        let url = provisioning_url(&s, "root").unwrap();
        assert!(url.starts_with("otpauth://totp/"));
        assert!(url.contains("server-monitor"));
        assert!(url.contains("root"));
    }
}
