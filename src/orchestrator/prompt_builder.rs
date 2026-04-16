//! PromptBuilder — renders worker prompts from Tera templates.
//!
//! The body of WORKFLOW.md (the text after the YAML front-matter `---` block)
//! is a Tera template. PromptBuilder renders it with task-specific context.
//!
//! Unknown variables are treated as errors. If a variable is referenced in the
//! template but not provided, the dispatch for that task is skipped and a warning
//! is posted to the interaction channels.
//!
//! Available template variables:
//!
//! | Variable                           | Description                          |
//! |------------------------------------|--------------------------------------|
//! | `product_name`                     | workflow.product                     |
//! | `issue.identifier`                 | task spec ID (e.g. "bd-a1b2")        |
//! | `issue.title`                      | task title                           |
//! | `issue.acceptance_criteria`        | acceptance criteria (required)       |
//! | `issue.context`                    | additional context from task author  |
//! | `issue.labels`                     | list of label strings                |
//! | `run.attempt`                      | current dispatch attempt number      |
//! | `run.model`                        | model that will be used for this run |

use crate::core::error::ThalaError;
use crate::core::task::TaskRecord;
use crate::core::workflow::WorkflowConfig;

// ── PromptBuilder ─────────────────────────────────────────────────────────────

pub struct PromptBuilder {
    template: String,
}

impl PromptBuilder {
    /// Construct a builder from a Tera template string.
    ///
    /// This is typically the body of WORKFLOW.md after the front-matter block.
    pub fn new(template: impl Into<String>) -> Self {
        Self {
            template: template.into(),
        }
    }

    /// Render the worker prompt for a task.
    ///
    /// Fails if the template references a variable that is not provided.
    pub fn render(
        &self,
        record: &TaskRecord,
        workflow: &WorkflowConfig,
        model: &str,
    ) -> Result<String, ThalaError> {
        let mut tera = tera::Tera::default();
        // Strict undefined: unknown variables are errors, not empty strings.
        tera.add_raw_template("prompt", &self.template)
            .map_err(|e| {
                ThalaError::WorkflowConfig(format!("WORKFLOW.md template parse error: {e}"))
            })?;

        let mut ctx = tera::Context::new();
        ctx.insert("product_name", &workflow.product);
        ctx.insert(
            "issue",
            &serde_json::json!({
                "identifier": record.spec.id.as_str(),
                "title": record.spec.title,
                "acceptance_criteria": record.spec.acceptance_criteria,
                "context": record.spec.context,
                "labels": record.spec.labels,
            }),
        );
        ctx.insert(
            "run",
            &serde_json::json!({
                "attempt": record.attempt,
                "model": model,
            }),
        );

        tera.render("prompt", &ctx).map_err(|e| {
            ThalaError::WorkflowConfig(format!(
                "WORKFLOW.md template render error for task {}: {e}",
                record.spec.id.as_str()
            ))
        })
    }
}

/// Extract the Tera template body from a WORKFLOW.md string.
///
/// Looks for content after the closing `---` or `...` of the YAML front-matter.
/// If no front-matter is present, returns the entire string as the template.
pub fn extract_template_body(content: &str) -> &str {
    // Expect front matter to start with "---" on the first line.
    let after_first = match content.find('\n') {
        Some(pos) if content[..pos].trim() == "---" => &content[pos + 1..],
        _ => return content,
    };

    // Find the closing delimiter.
    for delim in ["---", "..."] {
        if let Some(close_pos) = after_first.find(&format!("\n{delim}")) {
            let after_close = close_pos + 1 + delim.len();
            if after_close < after_first.len() {
                // Skip the closing delimiter line and any leading newline.
                let body = &after_first[after_close..];
                return body.trim_start_matches('\n');
            }
        }
    }

    // No closing delimiter found — the whole content after the first "---" is the template.
    after_first
}

// ── Fallback builder ──────────────────────────────────────────────────────────

/// Build a minimal worker prompt when no WORKFLOW.md template is available.
///
/// Used as a fallback when WORKFLOW.md does not have a Tera template body.
pub fn fallback_prompt(record: &TaskRecord) -> String {
    format!(
        "# Task: {id}\n\n\
         ## Title\n{title}\n\n\
         ## Acceptance Criteria\n{ac}\n\n\
         ## Context\n{ctx}\n\n\
         ---\nWrite DONE to `.thala/signals/{id}.signal` when complete.",
        id = record.spec.id.as_str(),
        title = record.spec.title,
        ac = record.spec.acceptance_criteria,
        ctx = record.spec.context,
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record(id: &str, title: &str, ac: &str) -> TaskRecord {
        use crate::core::ids::TaskId;
        use crate::core::task::TaskSpec;
        TaskRecord::new(TaskSpec {
            id: TaskId::new(id),
            title: title.into(),
            acceptance_criteria: ac.into(),
            context: "some context".into(),
            beads_ref: id.into(),
            model_override: None,
            always_human_review: false,
            labels: vec!["frontend".into()],
        })
    }

    fn make_workflow(product: &str) -> WorkflowConfig {
        serde_yaml::from_str(&format!(
            "product: \"{product}\"\ngithub_repo: \"org/repo\"\nexecution:\n  backend: Local\nlimits:\n  max_concurrent_runs: 3\nmodels:\n  worker: \"kimi-k2.5\"\n  manager: \"claude-opus-4-6\"\nretry:\n  max_attempts: 3\nmerge:\n  auto_merge: false\nstuck:\nhooks:\n"
        ))
        .unwrap()
    }

    #[test]
    fn renders_basic_template() {
        let record = make_record("bd-0001", "Fix the bug", "Bug is fixed");
        let workflow = make_workflow("example-app");
        let builder = PromptBuilder::new(
            "You work on {{ product_name }}. Task: {{ issue.identifier }} — {{ issue.title }}",
        );
        let rendered = builder.render(&record, &workflow, "kimi-k2.5").unwrap();
        assert!(rendered.contains("example-app"));
        assert!(rendered.contains("bd-0001"));
        assert!(rendered.contains("Fix the bug"));
    }

    #[test]
    fn unknown_variable_is_error() {
        let record = make_record("bd-0002", "X", "Y");
        let workflow = make_workflow("test");
        let builder = PromptBuilder::new("{{ nonexistent_var }}");
        let result = builder.render(&record, &workflow, "kimi");
        assert!(result.is_err());
    }

    #[test]
    fn extract_template_body_from_workflow_md() {
        let content = "---\nproduct: foo\n---\nYou work on {{ product_name }}.";
        let body = extract_template_body(content);
        assert_eq!(body, "You work on {{ product_name }}.");
    }

    #[test]
    fn extract_template_body_no_front_matter() {
        let content = "Just a plain template {{ issue.title }}";
        let body = extract_template_body(content);
        assert_eq!(body, content);
    }
}
