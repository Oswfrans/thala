//! Validation outcome types.
//!
//! Validation runs after a worker completes. Validators include:
//! - Review AI: LLM reviews the diff against acceptance criteria
//! - CI checks: GitHub Actions / CI system status
//! - Human check: explicit human approval before merge

use serde::{Deserialize, Serialize};

use crate::core::ids::RunId;

// ── ValidatorKind ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidatorKind {
    /// LLM reviews the code diff against acceptance criteria.
    ReviewAi,

    /// CI pipeline checks (GitHub Actions, etc.).
    CiChecks,

    /// Explicit human approval via an interaction channel.
    HumanApproval,

    /// No-op validator — always passes. Used when no real validator is configured.
    Noop,
}

impl ValidatorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReviewAi => "review_ai",
            Self::CiChecks => "ci_checks",
            Self::HumanApproval => "human_approval",
            Self::Noop => "noop",
        }
    }
}

// ── ValidationOutcome ─────────────────────────────────────────────────────────

/// The result of one validation step applied to a completed run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationOutcome {
    pub run_id: RunId,
    pub validator: ValidatorKind,
    pub passed: bool,

    /// Short summary of the outcome (used in notifications).
    pub summary: String,

    /// Detailed feedback (injected into retry prompt when passed = false).
    pub detail: Option<String>,
}

impl ValidationOutcome {
    pub fn pass(run_id: RunId, validator: ValidatorKind, summary: impl Into<String>) -> Self {
        Self {
            run_id,
            validator,
            passed: true,
            summary: summary.into(),
            detail: None,
        }
    }

    pub fn fail(
        run_id: RunId,
        validator: ValidatorKind,
        summary: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            run_id,
            validator,
            passed: false,
            summary: summary.into(),
            detail: Some(detail.into()),
        }
    }
}
