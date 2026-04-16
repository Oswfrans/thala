//! Shared LLM-based task planner for intake adapters.
//!
//! Both the Slack and Discord intake adapters use the same LLM call to extract
//! structured task fields (title, acceptance criteria, priority) from a
//! free-form message. This module provides the shared implementation.

use crate::core::error::ThalaError;

// ── PlannedTask ───────────────────────────────────────────────────────────────

/// Output of the planning LLM call.
#[derive(Debug, serde::Deserialize)]
pub struct PlannedTask {
    pub title: String,
    pub acceptance_criteria: String,
    pub priority: Option<String>,
}

// ── TaskPlanner ───────────────────────────────────────────────────────────────

/// Calls the manager LLM to extract structured task fields from a free-form message.
pub struct TaskPlanner {
    api_key: String,
    api_base: String,
    model: String,
    http: reqwest::Client,
}

impl TaskPlanner {
    pub fn new(
        api_key: impl Into<String>,
        api_base: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            api_base: api_base.into(),
            model: model.into(),
            http: reqwest::Client::new(),
        }
    }

    /// Extract a structured task from a free-form message.
    ///
    /// Returns a `PlannedTask` with the extracted fields.
    /// Fails if the LLM call fails or the response cannot be parsed as JSON.
    pub async fn plan(&self, message: &str) -> Result<PlannedTask, ThalaError> {
        let prompt = format!(
            r"Extract a software task from this message. Respond with JSON only.

Fields:
- title: short action phrase (required)
- acceptance_criteria: specific, testable criteria (required)
- priority: one of P0, P1, P2, P3 (optional, default P2)

Message: {message}

JSON:"
        );

        let response = self
            .http
            .post(format!("{}/chat/completions", self.api_base))
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({
                "model": self.model,
                "messages": [{"role": "user", "content": prompt}],
                "response_format": {"type": "json_object"},
                "max_tokens": 512,
            }))
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| ThalaError::interaction(format!("LLM planning call failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(ThalaError::interaction(format!(
                "LLM planning API returned {status}: {text}"
            )));
        }

        let data: serde_json::Value = response
            .json()
            .await
            .map_err(|e| ThalaError::interaction(format!("Failed to parse LLM response: {e}")))?;

        let content = data["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("{}");

        serde_json::from_str::<PlannedTask>(content).map_err(|e| {
            ThalaError::interaction(format!(
                "Failed to parse planned task from LLM output: {e}\nRaw: {content}"
            ))
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planned_task_deserializes() {
        let json = r#"{"title":"Fix login button","acceptance_criteria":"Button is centered","priority":"P1"}"#;
        let task: PlannedTask = serde_json::from_str(json).unwrap();
        assert_eq!(task.title, "Fix login button");
        assert_eq!(task.acceptance_criteria, "Button is centered");
        assert_eq!(task.priority.as_deref(), Some("P1"));
    }

    #[test]
    fn planned_task_no_priority() {
        let json = r#"{"title":"X","acceptance_criteria":"Y"}"#;
        let task: PlannedTask = serde_json::from_str(json).unwrap();
        assert!(task.priority.is_none());
    }
}
