//! Thala's adapter implementations.
//!
//! All I/O lives here. The orchestrator depends only on port traits (src/ports/).
//! The adapters in this module are the only code that touches external systems.
//!
//! Boundary summary:
//!
//!   Beads (task truth)
//!     reads:  adapters/beads/source.rs  →  ports::TaskSource
//!     writes: adapters/beads/sink.rs    →  ports::TaskSink
//!
//!   Slack intake (message → Beads task)
//!     adapters/intake/slack.rs          →  ports::TaskSink
//!
//!   Discord intake (message → Beads task)
//!     adapters/intake/discord.rs        →  ports::TaskSink
//!
//!   Slack interaction (human approvals)
//!     adapters/interaction/slack.rs     →  ports::InteractionLayer
//!
//!   Discord interaction (human approvals)
//!     adapters/interaction/discord.rs   →  ports::InteractionLayer
//!
//!   Execution backends
//!     adapters/execution/local.rs       →  ports::ExecutionBackend
//!     adapters/execution/modal.rs       →  ports::ExecutionBackend
//!     adapters/execution/cloudflare.rs  →  ports::ExecutionBackend
//!     adapters/execution/router.rs      →  ports::BackendRouter
//!
//!   State persistence
//!     adapters/state/mod.rs             →  ports::StateStore
//!
//!   Git / GitHub operations
//!     adapters/repo/mod.rs              →  ports::RepoProvider
//!
//!   Validation
//!     adapters/validation/noop.rs       →  ports::Validator
//!     adapters/validation/review_ai.rs  →  ports::Validator

pub mod beads;
pub mod execution;
pub mod intake;
pub mod interaction;
pub mod repo;
pub mod state;
pub mod validation;
