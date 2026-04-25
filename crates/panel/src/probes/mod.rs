//! Probe result ingestion + rollup + scheduler exports.
//!
//! Mirror of [`crate::metrics`] for network probes. The shape is parallel by
//! design: agents push `ProbeBatch` over the same gRPC stream that carries
//! metrics, panel persists them in `probe_results` with `granularity='raw'`,
//! the rollup task aggregates upward (m1 → m5 → h1) computing success rate +
//! p50/p95 latency, and a retention prune drops aged rows per tier.

pub mod ingest;
pub mod rollup;
pub mod scheduler;

pub use ingest::ingest_batch;
pub use scheduler::{AssignmentBus, Scheduler};
