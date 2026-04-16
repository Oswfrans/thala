//! Discord intake adapter — translates Discord messages into Beads tasks.
//!
//! Same flow as SlackIntake but for Discord:
//!   1. Receive a Discord message (webhook or gateway event).
//!   2. Call the planning LLM to structure it.
//!   3. Write the structured task to Beads via TaskSink.
//!   4. Reply to the Discord message.
//!
//! # Example: appending context from Discord
//!
//! User posts: "/thala context bd-a1b2 This also affects the checkout page"
//! → DiscordIntake.append_context() is called
//! → BeadsTaskSink.append_context(task_id, context) is called
//! → Reply: "Added context to task bd-a1b2"
//!
//! Boundary rule: this adapter writes to Beads and does not read from
//! Thala's orchestrator state. InteractionLayer (approvals, retries) is
//! handled by the separate DiscordInteraction adapter.

use std::sync::Arc;

use crate::adapters::intake::planner::TaskPlanner;
use crate::core::error::ThalaError;
use crate::ports::task_sink::{NewTaskRequest, TaskSink};

// ── DiscordIntakeConfig ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DiscordIntakeConfig {
    /// Discord bot token.
    pub bot_token: String,

    /// Discord application public key for verifying interaction signatures.
    pub public_key: String,

    /// Manager model API key.
    pub manager_api_key: String,

    /// Manager model API base URL.
    pub manager_api_base: String,

    /// Planning model (e.g. "anthropic/claude-opus-4-6").
    pub planning_model: String,

    /// Product name written to new task labels.
    pub product: String,
}

// ── DiscordIntakeMessage ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DiscordIntakeMessage {
    pub channel_id: String,
    pub user_id: String,
    pub guild_id: Option<String>,
    pub content: String,
    /// Discord message ID for reply threading.
    pub message_id: String,
}

// ── DiscordIntake ─────────────────────────────────────────────────────────────

pub struct DiscordIntake {
    config: DiscordIntakeConfig,
    sink: Arc<dyn TaskSink>,
    planner: TaskPlanner,
}

impl DiscordIntake {
    pub fn new(config: DiscordIntakeConfig, sink: Arc<dyn TaskSink>) -> Self {
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

    /// Handle a Discord message that requests task creation.
    pub async fn handle_create(&self, msg: DiscordIntakeMessage) -> String {
        match self.run_create(&msg).await {
            Ok(reply) => reply,
            Err(e) => {
                tracing::error!(
                    user = %msg.user_id,
                    channel = %msg.channel_id,
                    "Discord intake failed: {e}"
                );
                format!("Sorry, I couldn't create that task: {e}")
            }
        }
    }

    /// Handle a Discord message that appends context to an existing task.
    ///
    /// # Example
    ///
    /// Command: `/thala context bd-a1b2 Also affects checkout`
    pub async fn handle_append_context(
        &self,
        task_id: &str,
        context: &str,
        msg: &DiscordIntakeMessage,
    ) -> String {
        match self
            .sink
            .append_context(
                task_id,
                &format!("{context}\n\n_via Discord <@{}>_", msg.user_id),
            )
            .await
        {
            Ok(()) => format!("Added context to task `{task_id}`"),
            Err(e) => format!("Failed to add context: {e}"),
        }
    }

    async fn run_create(&self, msg: &DiscordIntakeMessage) -> Result<String, ThalaError> {
        let planned = self.plan_task(&msg.content).await?;

        let task_id = self
            .sink
            .create_task(NewTaskRequest {
                title: planned.title.clone(),
                acceptance_criteria: planned.acceptance_criteria.clone(),
                context: format!(
                    "Submitted via Discord by <@{}> in channel {}",
                    msg.user_id, msg.channel_id
                ),
                priority: planned.priority.clone(),
                labels: vec![self.config.product.clone(), "intake:discord".into()],
                submitted_by: format!(
                    "discord:{}:{}",
                    msg.guild_id.as_deref().unwrap_or("dm"),
                    msg.user_id
                ),
                always_human_review: false,
            })
            .await?;

        Ok(format!(
            "Created task `{task_id}`: **{}**\n*Acceptance criteria:* {}",
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
