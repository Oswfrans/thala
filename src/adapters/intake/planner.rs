//! Shared LLM-based task planner for intake adapters.
//!
//! Both the Slack and Discord intake adapters use the same LLM call to extract
//! structured task fields (title, acceptance criteria, priority) from a
//! free-form message. This module provides the shared implementation.

use crate::core::error::ThalaError;
use serde::Deserialize;
use serde_json::Value;

// ── PlannedTask ───────────────────────────────────────────────────────────────

/// Output of the planning LLM call.
#[derive(Debug, serde::Deserialize)]
pub struct PlannedTask {
    pub title: String,
    #[serde(deserialize_with = "deserialize_acceptance_criteria")]
    pub acceptance_criteria: String,
    pub priority: Option<String>,
}

fn deserialize_acceptance_criteria<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::String(text) => Ok(text),
        Value::Array(items) => {
            let criteria = items
                .into_iter()
                .map(|item| match item {
                    Value::String(text) => Ok(text),
                    other => Err(serde::de::Error::custom(format!(
                        "acceptance_criteria array items must be strings, got {other}"
                    ))),
                })
                .collect::<Result<Vec<_>, D::Error>>()?;

            Ok(criteria
                .into_iter()
                .map(|criterion| format!("- {criterion}"))
                .collect::<Vec<_>>()
                .join("\n"))
        }
        other => Err(serde::de::Error::custom(format!(
            "acceptance_criteria must be a string or string array, got {other}"
        ))),
    }
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
                "max_tokens": 512,
                "temperature": 0.0,
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

        let content = extract_message_content(&data)?;
        let json = extract_json_object(&content)?;

        serde_json::from_str::<PlannedTask>(json).map_err(|e| {
            ThalaError::interaction(format!(
                "Failed to parse planned task from LLM output: {e}\nRaw: {}",
                truncate_for_error(&content)
            ))
        })
    }
}

fn extract_message_content(data: &Value) -> Result<String, ThalaError> {
    let finish_reason = data
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
        .unwrap_or("unknown");

    let Some(content) = data.pointer("/choices/0/message/content") else {
        return Err(ThalaError::interaction(format!(
            "LLM planning response did not include choices[0].message.content \
             (finish_reason: {finish_reason})"
        )));
    };

    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| part.get("content").and_then(Value::as_str))
            })
            .collect::<String>(),
        _ => {
            return Err(ThalaError::interaction(format!(
                "LLM planning response content had unexpected type \
                 (finish_reason: {finish_reason})"
            )));
        }
    };

    if text.trim().is_empty() {
        return Err(ThalaError::interaction(format!(
            "LLM planning response was empty (finish_reason: {finish_reason})"
        )));
    }

    Ok(text)
}

fn extract_json_object(content: &str) -> Result<&str, ThalaError> {
    let trimmed = content.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Ok(trimmed);
    }

    let without_fence = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|inner| inner.strip_suffix("```"))
        .map(str::trim);

    if let Some(json) = without_fence {
        if json.starts_with('{') && json.ends_with('}') {
            return Ok(json);
        }
    }

    let Some(start) = trimmed.find('{') else {
        return Err(ThalaError::interaction(format!(
            "LLM planning response did not contain a JSON object. Raw: {}",
            truncate_for_error(content)
        )));
    };
    let Some(end) = trimmed.rfind('}') else {
        return Err(ThalaError::interaction(format!(
            "LLM planning response did not contain a complete JSON object. Raw: {}",
            truncate_for_error(content)
        )));
    };

    if start >= end {
        return Err(ThalaError::interaction(format!(
            "LLM planning response did not contain a valid JSON object. Raw: {}",
            truncate_for_error(content)
        )));
    }

    Ok(&trimmed[start..=end])
}

fn truncate_for_error(text: &str) -> String {
    const MAX_CHARS: usize = 1_000;

    let mut chars = text.chars();
    let truncated = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
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

    #[test]
    fn planned_task_accepts_acceptance_criteria_array() {
        let json = r#"{"title":"X","acceptance_criteria":["First criterion","Second criterion"],"priority":"P1"}"#;

        let task: PlannedTask = serde_json::from_str(json).unwrap();

        assert_eq!(
            task.acceptance_criteria,
            "- First criterion\n- Second criterion"
        );
    }

    #[test]
    fn extracts_string_content_from_chat_response() {
        let data = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "{\"title\":\"X\",\"acceptance_criteria\":\"Y\"}"
                },
                "finish_reason": "stop"
            }]
        });

        let content = extract_message_content(&data).unwrap();

        assert_eq!(content, r#"{"title":"X","acceptance_criteria":"Y"}"#);
    }

    #[test]
    fn extracts_array_content_from_chat_response() {
        let data = serde_json::json!({
            "choices": [{
                "message": {
                    "content": [
                        {"type": "text", "text": "{\"title\":\"X\","},
                        {"type": "text", "text": "\"acceptance_criteria\":\"Y\"}"}
                    ]
                },
                "finish_reason": "stop"
            }]
        });

        let content = extract_message_content(&data).unwrap();

        assert_eq!(content, r#"{"title":"X","acceptance_criteria":"Y"}"#);
    }

    #[test]
    fn rejects_empty_chat_response_content() {
        let data = serde_json::json!({
            "choices": [{
                "message": {"content": ""},
                "finish_reason": "stop"
            }]
        });

        let error = extract_message_content(&data).unwrap_err().to_string();

        assert!(error.contains("LLM planning response was empty"));
    }

    #[test]
    fn extracts_json_from_fenced_content() {
        let content = "```json\n{\"title\":\"X\",\"acceptance_criteria\":\"Y\"}\n```";

        let json = extract_json_object(content).unwrap();

        assert_eq!(json, r#"{"title":"X","acceptance_criteria":"Y"}"#);
    }
}
