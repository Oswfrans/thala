//! Validation adapters.
//!
//! - [`ReviewAiValidator`] — calls the manager LLM to review a completed run's diff.
//! - [`NoopValidator`]    — always passes (useful for testing and local-only setups).

pub mod noop;
pub mod review_ai;

pub use noop::NoopValidator;
pub use review_ai::ReviewAiValidator;
