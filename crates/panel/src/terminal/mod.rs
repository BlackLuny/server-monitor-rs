//! Terminal session bookkeeping + recording streaming.

mod hub;
pub mod recordings;

pub use hub::{ClosedInfo, Frame, TerminalHub, MAX_SESSIONS_PER_USER};
pub use recordings::{RecordingFetchError, RecordingHub};
