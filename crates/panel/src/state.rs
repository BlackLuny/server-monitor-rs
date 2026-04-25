//! Shared application state passed to Axum handlers and gRPC services.

use sqlx::PgPool;

use crate::{grpc::SessionHub, live::LiveBus};

/// Injected into every handler via `State<AppState>`. Cheap to clone — every
/// field is already reference-counted internally (`PgPool` holds an `Arc`,
/// `SessionHub` wraps `Arc<DashMap>`, `LiveBus` wraps a broadcast channel).
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub hub: SessionHub,
    pub live: LiveBus,
    /// Whether session cookies should carry the `Secure` attribute.
    /// `false` is only correct for plain-HTTP local/dev deployments — the
    /// default elsewhere is `true`. Tests don't care, so the bare `new`
    /// constructor leaves this off; production wiring sets it explicitly.
    pub cookies_secure: bool,
}

impl AppState {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            hub: SessionHub::new(),
            live: LiveBus::new(),
            cookies_secure: false,
        }
    }

    #[must_use]
    pub fn with_cookies_secure(mut self, secure: bool) -> Self {
        self.cookies_secure = secure;
        self
    }
}
