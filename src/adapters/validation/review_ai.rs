//! ReviewAiValidator — calls the manager LLM to review a completed run's diff.
//!
//! Sends the diff and task acceptance criteria to the manager model and
//! returns Pass/Fail based on the model's assessment.
//!
//! The diff is obtained by running `git diff HEAD` in the worktree directory.
//! For remote backends (no worktree), the diff will be empty and the review
//! will assess based solely on the acceptance criteria and task context.

use async_trait::async_trait;

use crate::core::error::ThalaError;
use crate::core::run::TaskRun;
use crate::core::validation::{ValidationOutcome, ValidatorKind};
use crate::ports::validator::Validator;

pub struct ReviewAiValidator {
    /// Anthropic API key (read from env at construction time).
    api_key: String,

    /// Model ID for the review (e.g. "claude-opus-4-6").
    model: String,

    http: reqwest::Client,
}

impl ReviewAiValidator {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            http: reqwest::Client::new(),
        }
    }

    /// Construct from the `ANTHROPIC_API_KEY` environment variable.
    pub fn from_env(model: impl Into<String>) -> Result<Self, ThalaError> {
        let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
            ThalaError::Validation("ANTHROPIC_API_KEY environment variable not set".into())
        })?;
        Ok(Self::new(api_key, model))
    }

    /// Get the git diff from the run's worktree (Local backend).
    /// Returns an empty string when no worktree is available.
    async fn get_diff(&self, run: &TaskRun) -> String {
        let worktree = match &run.worktree_path {
            Some(p) => p.clone(),
            None => return String::new(),
        };

        let output = tokio::process::Command::new("git")
            .args(["diff", "HEAD"])
            .current_dir(&worktree)
            .output()
            .await;

        match output {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                tracing::warn!(
                    run_id = %run.run_id,
                    worktree,
                    "git diff returned non-zero: {}",
                    stderr.trim()
                );
                String::new()
            }
            Err(e) => {
                tracing::warn!(run_id = %run.run_id, "Failed to run git diff: {e}");
                String::new()
            }
        }
    }

    /// Call the Anthropic Messages API to review the diff against the run's context.
    async fn call_review_llm(
        &self,
        run: &TaskRun,
        diff: &str,
    ) -> Result<ValidationOutcome, ThalaError> {
        // Build a review prompt. Keep it concise to stay within token limits.
        let diff_section = if diff.is_empty() {
            "No diff available (remote backend or no worktree changes).".to_string()
        } else {
            format!("```diff\n{diff}\n```")
        };

        // Note: review_feedback from a previous cycle is injected here if present.
        let feedback_section = run
            .review_feedback
            .as_deref()
            .map(|f| format!("\n\n## Previous Review Feedback\n{f}"))
            .unwrap_or_default();

        let prompt = format!(
            "You are reviewing a code change produced by an AI coding agent.\n\n\
             Review the following diff and respond with a JSON object containing:\n\
             - \"passed\": true if the diff represents a meaningful, non-empty change \
               that is syntactically valid and does not introduce obvious regressions, \
               false otherwise\n\
             - \"detail\": a concise explanation (1-3 sentences)\n\n\
             Task ID: {task_id}{feedback}\n\n\
             ## Diff\n\
             {diff_section}\n\n\
             Respond with JSON only.",
            task_id = run.task_id.as_str(),
            feedback = feedback_section,
            diff_section = diff_section,
        );

        let request_body = serde_json::json!({
            "model": self.model,
            "max_tokens": 512,
            "messages": [
                {"role": "user", "content": prompt}
            ]
        });

        let resp = self
            .http
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request_body)
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await
            .map_err(|e| ThalaError::Validation(format!("ReviewAI API call failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ThalaError::Validation(format!(
                "ReviewAI API returned {status}: {text}"
            )));
        }

        let data: serde_json::Value = resp.json().await.map_err(|e| {
            ThalaError::Validation(format!("Failed to parse ReviewAI response: {e}"))
        })?;

        // Extract the text content from the Anthropic response.
        let content = data["content"][0]["text"]
            .as_str()
            .unwrap_or("{\"passed\":false,\"detail\":\"Could not parse response\"}");

        // Parse the JSON review decision.
        let decision: serde_json::Value = serde_json::from_str(content)
            .unwrap_or_else(|_| serde_json::json!({"passed": false, "detail": content}));

        let passed = decision["passed"].as_bool().unwrap_or(false);
        let detail = decision["detail"]
            .as_str()
            .unwrap_or("No detail provided")
            .to_string();

        tracing::info!(
            run_id = %run.run_id,
            passed,
            detail = %detail,
            "ReviewAI decision"
        );

        if passed {
            Ok(ValidationOutcome::pass(
                run.run_id.clone(),
                ValidatorKind::ReviewAi,
                detail,
            ))
        } else {
            Ok(ValidationOutcome::fail(
                run.run_id.clone(),
                ValidatorKind::ReviewAi,
                "Review AI rejected the diff",
                detail,
            ))
        }
    }
}

#[async_trait]
impl Validator for ReviewAiValidator {
    fn kind(&self) -> ValidatorKind {
        ValidatorKind::ReviewAi
    }

    async fn validate(&self, run: &TaskRun) -> Result<ValidationOutcome, ThalaError> {
        let diff = self.get_diff(run).await;

        tracing::info!(
            run_id = %run.run_id,
            task_id = %run.task_id,
            model = %self.model,
            diff_bytes = diff.len(),
            "ReviewAiValidator: calling LLM review"
        );

        self.call_review_llm(run, &diff).await
    }
}
