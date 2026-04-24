//! Build-time version information.

/// Crate / binary version string (from `Cargo.toml`'s `[package.version]`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Package name of the binary that linked this crate (for log prefixes etc.).
pub fn pkg_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}
