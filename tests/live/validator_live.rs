//! Live tests for AI-powered validators.
//!
//! These tests require valid LLM API credentials.
//! Run with: cargo test --test test_live -- --ignored

use thala::adapters::validation::review_ai::ReviewAiValidator;
use thala::core::ids::{RunId, TaskId};
use thala::core::run::{ExecutionBackendKind, TaskRun};
use thala::core::task::TaskSpec;
use thala::core::validation::ValidatorKind;
use thala::ports::validator::Validator;

fn dummy_spec(id: &str) -> TaskSpec {
    TaskSpec {
        id: TaskId::new(id),
        title: "Live test task".into(),
        acceptance_criteria: "The implementation should be correct and well-tested.".into(),
        context: String::new(),
        beads_ref: id.into(),
        model_override: None,
        always_human_review: false,
        labels: vec![],
    }
}

/// Test that ReviewAiValidator actually calls the LLM when ANTHROPIC_API_KEY is set.
#[tokio::test]
#[ignore = "requires ANTHROPIC_API_KEY"]
async fn review_ai_validator_live_calls_llm() {
    let validator = ReviewAiValidator::from_env("claude-sonnet-4-6").unwrap();
    assert_eq!(validator.kind(), ValidatorKind::ReviewAi);

    let run = TaskRun::new(
        RunId::new_v4(),
        TaskId::new("bd-live-001"),
        1,
        ExecutionBackendKind::Local,
    );
    let spec = dummy_spec("bd-live-001");

    let outcome = validator.validate(&run, &spec).await.unwrap();

    assert_eq!(outcome.validator, ValidatorKind::ReviewAi);
    assert!(!outcome.summary.is_empty());
}

/// Test that ReviewAiValidator validates with explicit API key.
#[tokio::test]
#[ignore = "requires ANTHROPIC_API_KEY"]
async fn review_ai_validator_with_explicit_key() {
    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY not set");

    let validator = ReviewAiValidator::new(api_key, "claude-sonnet-4-6");

    let run = TaskRun::new(
        RunId::new_v4(),
        TaskId::new("bd-live-002"),
        1,
        ExecutionBackendKind::Local,
    );
    let spec = dummy_spec("bd-live-002");

    let outcome = validator.validate(&run, &spec).await.unwrap();
    assert_eq!(outcome.validator, ValidatorKind::ReviewAi);
}
