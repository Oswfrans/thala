//! Validator port — validate a completed run.
//!
//! Validators are invoked by the validator coordinator after a run completes.
//! Multiple validators may run for one run; all must pass for the task to succeed.

use async_trait::async_trait;

use crate::core::error::ThalaError;
use crate::core::run::TaskRun;
use crate::core::task::TaskSpec;
use crate::core::validation::{ValidationOutcome, ValidatorKind};

// ── Validator ─────────────────────────────────────────────────────────────────

/// Validates the output of a completed run.
///
/// Implementations:
/// - ReviewAiValidator (adapters/validation/review_ai.rs)
/// - CiValidator (adapters/validation/ci.rs)
///
/// Human approval is handled by the InteractionLayer + HumanLoop, not here.
#[async_trait]
pub trait Validator: Send + Sync {
    /// Which kind of validation this validator performs.
    fn kind(&self) -> ValidatorKind;

    /// Run validation against a completed run.
    ///
    /// The run must be in a terminal status (Completed) before this is called.
    /// The spec provides the task's acceptance criteria for review AI validators.
    async fn validate(
        &self,
        run: &TaskRun,
        spec: &TaskSpec,
    ) -> Result<ValidationOutcome, ThalaError>;
}
