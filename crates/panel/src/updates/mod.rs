//! Agent self-update orchestration (M7).
//!
//! Three sub-modules:
//!   - [`poller`] periodically asks GitHub for the latest release of the
//!     configured repo and caches the asset list (with sha256 hashes
//!     parsed out of the release's SHA256SUMS file).
//!   - [`rollout`] turns an admin-driven create-rollout request into a set
//!     of `update_assignments` rows and walks the rollout state machine
//!     (pending → active → completed | paused | aborted).
//!   - The agent stream handler consumes [`AssignmentUpdate`] events to
//!     keep `update_assignments.state` fresh as agents report progress.

pub mod dispatch;
pub mod poller;
pub mod rollout;

pub use poller::{LatestRelease, ReleaseAsset};
pub use rollout::{
    abort_rollout, agent_target_triple, create_rollout, get_rollout, list_recent_releases,
    list_rollouts, pause_rollout, resume_rollout, AgentFilter, CreateRolloutInput, RolloutSummary,
    RolloutView,
};
