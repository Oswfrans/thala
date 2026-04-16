//! Slack intake adapter — translates Slack messages into Beads tasks.
//!
//! Flow:
//!   1. Receive a Slack message (from Events API or slash command).
//!   2. Call the planning LLM to structure it into title + acceptance criteria.
//!   3. Write the structured task to Beads via TaskSink.
//!   4. Reply to the Slack message confirming creation.
//!
//! Boundary rule: this adapter does NOT implement InteractionLayer.
//! Intake (create tasks) and interaction (approve/reject/retry) are separate.
//! This adapter writes to Beads; it does not read from the orchestrator state.

use std::sync::Arc;

use crate::adapters::intake::planner::TaskPlanner;
use crate::core::error::ThalaError;
use crate::ports::task_sink::{NewTaskRequest, TaskSink};

// ── SlackIntakeConfig ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SlackIntakeConfig {
    /// Slack bot token (xoxb-…).
    pub bot_token: String,

    /// Slack signing secret for verifying request signatures.
    pub signing_secret: String,

    /// Manager model API key (OpenRouter or compatible).
    pub manager_api_key: String,

    /// Manager model API base URL.
    pub manager_api_base: String,

    /// Model to use for task planning (e.g. "anthropic/claude-opus-4-6").
    pub planning_model: String,

    /// Product name written to new task labels (e.g. "example-app").
    pub product: String,
}

// ── SlackIntakeMessage ────────────────────────────────────────────────────────

/// A message received from Slack that may be turned into a task.
#[derive(Debug, Clone)]
pub struct SlackIntakeMessage {
    pub channel_id: String,
    pub user_id: String,
    pub text: String,
    /// Thread timestamp for reply threading.
    pub thread_ts: Option<String>,
}

// ── SlackIntake ───────────────────────────────────────────────────────────────

pub struct SlackIntake {
    config: SlackIntakeConfig,
    sink: Arc<dyn TaskSink>,
    planner: TaskPlanner,
}

impl SlackIntake {
    pub fn new(config: SlackIntakeConfig, sink: Arc<dyn TaskSink>) -> Self {
        let planner = TaskPlanner::new(
            &config.manager_api_key,
            &config.manager_api_base,
            &config.planning_model,
        );
        Self {
            config,
            sink,
            planner,
        }
    }

    /// Handle one Slack message. Returns the reply text to send back.
    ///
    /// # Example: creating a task from Slack
    ///
    /// User posts: "Fix the login button alignment on mobile"
    /// → LLM plans: title="Fix login button alignment", AC="Button centered on mobile"
    /// → BeadsTaskSink.create_task() is called
    /// → Reply: "Created task bd-a1b2: Fix login button alignment"
    pub async fn handle(&self, msg: SlackIntakeMessage) -> String {
        match self.run(&msg).await {
            Ok(reply) => reply,
            Err(e) => {
                tracing::error!(
                    user = %msg.user_id,
                    channel = %msg.channel_id,
                    "Slack intake failed: {e}"
                );
                format!("Sorry, I couldn't create that task: {e}")
            }
        }
    }

    async fn run(&self, msg: &SlackIntakeMessage) -> Result<String, ThalaError> {
        // Step 1: Plan the task using the manager LLM.
        let planned = self.plan_task(&msg.text).await?;

        // Step 2: Write to Beads.
        let task_id = self
            .sink
            .create_task(NewTaskRequest {
                title: planned.title.clone(),
                acceptance_criteria: planned.acceptance_criteria.clone(),
                context: format!(
                    "Submitted via Slack by <@{}> in <#{}>",
                    msg.user_id, msg.channel_id
                ),
                priority: planned.priority.clone(),
                labels: vec![self.config.product.clone(), "intake:slack".into()],
                submitted_by: format!("slack:{}:{}", msg.channel_id, msg.user_id),
                always_human_review: false,
            })
            .await?;

        Ok(format!(
            "Created task `{task_id}`: *{}*\n_Acceptance criteria:_ {}",
            planned.title, planned.acceptance_criteria
        ))
    }

    async fn plan_task(
        &self,
        message: &str,
    ) -> Result<crate::adapters::intake::planner::PlannedTask, ThalaError> {
        self.planner.plan(message).await
    }
}
