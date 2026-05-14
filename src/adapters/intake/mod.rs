//! Intake adapters — translate Slack/Discord messages into Beads tasks.
//!
//! Intake is SEPARATE from interaction. Intake creates tasks; interaction
//! handles approvals, retries, and escalations on existing tasks.

pub mod discord;
pub mod discord_webhook;
pub mod planner;
pub mod slack;
pub mod slack_webhook;
