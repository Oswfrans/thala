//! Live tests for AI-powered validators.
//!
//! These tests require valid LLM API credentials.
//! Run with: cargo test --test test_live -- --ignored

use thala::adapters::validation::review_ai::ReviewAiValidator;
use thala::core::ids::{RunId, TaskId};
use thala::core::run::{ExecutionBackendKind, TaskRun};
use thala::core::validation::ValidatorKind;
use thala::ports::validator::Validator;

/// Test that ReviewAiValidator actually calls the LLM when ANTHROPIC_API_KEY is set.
#[tokio::test]
#[ignore = "requires ANTHROPIC_API_KEY"]
async fn review_ai_validator_live_calls_llm() {
    // Ensure API key is available
    let validator = ReviewAiValidator::from_env("claude-sonnet-4").unwrap();
    assert_eq!(validator.kind(), ValidatorKind::ReviewAi);

    // Create a run with PR information (in a real test, this would have actual PR data)
    let run = TaskRun::new(
        RunId::new_v4(),
        TaskId::new("bd-live-001"),
        1,
        ExecutionBackendKind::Local,
    );

    // This should actually call the Anthropic API
    let outcome = validator.validate(&run).await.unwrap();

    // The outcome should now be based on actual LLM review, not the stub
    // We can't assert on pass/fail since it depends on the actual review,
    // but we can verify the validator kind is preserved
    assert_eq!(outcome.validator, ValidatorKind::ReviewAi);
    assert!(!outcome.summary.is_empty());
}

/// Test that ReviewAiValidator validates with explicit API key.
#[tokio::test]
#[ignore = "requires ANTHROPIC_API_KEY"]
async fn review_ai_validator_with_explicit_key() {
    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY not set");

    let validator = ReviewAiValidator::new(api_key, "claude-sonnet-4");

    let run = TaskRun::new(
        RunId::new_v4(),
        TaskId::new("bd-live-002"),
        1,
        ExecutionBackendKind::Local,
    );

    let outcome = validator.validate(&run).await.unwrap();
    assert_eq!(outcome.validator, ValidatorKind::ReviewAi);
}
