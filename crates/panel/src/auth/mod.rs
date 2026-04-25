//! Authentication & authorization primitives (M3).
//!
//! Design notes:
//! - **Sessions are DB-backed**, not JWT. The cookie carries an opaque 32-byte
//!   session id; all validation hits `login_sessions`. Revocation is immediate.
//! - **Sliding 7-day expiry**: every successful validation bumps `last_used_at`,
//!   so active users stay logged in but idle ones age out.
//! - **CSRF is handled at the edge** via an `Origin`/`Referer` header check for
//!   mutating methods (see `csrf`). Combined with `SameSite=Strict` cookies
//!   this covers every modern-browser attack vector.
//! - **Password hashing** is Argon2id with default parameters (~100ms on a
//!   modern box). Backup codes reuse the same scheme so a DB dump can't
//!   replay them.

pub mod audit;
pub mod backup_codes;
pub mod cookie;
pub mod csrf;
pub mod extract;
pub mod password;
pub mod request;
pub mod session;
pub mod totp;

pub use extract::{AdminUser, AuthUser};
pub use request::session_meta;
pub use session::{Session, SessionMeta};
