//! NoopValidator — always passes validation.
//!
//! Useful for local/testing setups where AI review is not needed.

use async_trait::async_trait;

use crate::core::error::ThalaError;
use crate::core::run::TaskRun;
use crate::core::validation::{ValidationOutcome, ValidatorKind};
use crate::ports::validator::Validator;

pub struct NoopValidator;

#[async_trait]
impl Validator for NoopValidator {
    fn kind(&self) -> ValidatorKind {
        ValidatorKind::ReviewAi
    }

    async fn validate(&self, run: &TaskRun) -> Result<ValidationOutcome, ThalaError> {
        Ok(ValidationOutcome::pass(
            run.run_id.clone(),
            ValidatorKind::ReviewAi,
            "No validation configured (noop).",
        ))
    }
}
