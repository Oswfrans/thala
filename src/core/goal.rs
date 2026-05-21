//! Goal-mode runtime types.
//!
//! Goal records are Thala-owned orchestration state. Beads remains canonical
//! for the source task text; these records only track research fanout progress.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::ids::{GoalId, RunId, TaskId};
use crate::core::run::ExecutionBackendKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GoalStatus {
    Planning,
    Researching,
    Synthesizing,
    WaitingForHuman,
    CreatingFollowups,
    Succeeded,
    Failed,
    Resolved,
}

impl GoalStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Resolved)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalRecord {
    pub goal_id: GoalId,
    pub source_task_id: TaskId,
    pub title: String,
    pub objective: String,
    pub status: GoalStatus,
    pub research_round: u32,
    pub max_rounds: u32,
    #[serde(default)]
    pub synthesis_decision: Option<GoalSynthesisDecision>,
    #[serde(default)]
    pub created_followup_titles: Vec<String>,
    #[serde(default)]
    pub created_followup_ids: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl GoalRecord {
    pub fn new(
        goal_id: GoalId,
        source_task_id: TaskId,
        title: String,
        objective: String,
        max_rounds: u32,
    ) -> Self {
        let now = Utc::now();
        Self {
            goal_id,
            source_task_id,
            title,
            objective,
            status: GoalStatus::Planning,
            research_round: 0,
            max_rounds,
            synthesis_decision: None,
            created_followup_titles: Vec::new(),
            created_followup_ids: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResearchRunStatus {
    Planned,
    Running,
    Completed,
    Failed,
}

impl ResearchRunStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchRunFailure {
    pub reason: String,
}

impl ResearchRunFailure {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchRun {
    pub research_run_id: RunId,
    pub goal_id: GoalId,
    pub source_task_id: TaskId,
    pub child_task_id: TaskId,
    pub variant_name: String,
    pub prompt: String,
    pub backend: ExecutionBackendKind,
    pub worker_run_id: RunId,
    pub status: ResearchRunStatus,
    #[serde(default)]
    pub research_round: u32,
    pub report_artifact: Option<String>,
    #[serde(default)]
    pub report: Option<ResearchReport>,
    #[serde(default)]
    pub failure: Option<ResearchRunFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchReport {
    pub goal_id: String,
    pub run_id: String,
    pub variant: String,
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<ResearchFinding>,
    pub recommendation: String,
    #[serde(default)]
    pub risks: Vec<String>,
    #[serde(default)]
    pub follow_up_questions: Vec<String>,
    #[serde(default)]
    pub suggested_tasks: Vec<SuggestedTask>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchFinding {
    pub claim: String,
    pub evidence: String,
    pub source: Option<String>,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestedTask {
    pub title: String,
    pub acceptance_criteria: String,
    pub context: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalDecisionKind {
    Done,
    AskHuman,
    CreateTasks,
    ResearchMore,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalSynthesisDecision {
    pub decision: GoalDecisionKind,
    pub summary: String,
    pub human_question: Option<String>,
    #[serde(default)]
    pub follow_up_tasks: Vec<SuggestedTask>,
    pub reason: String,
}
