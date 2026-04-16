//! StateStore port — persist Thala's runtime state.
//!
//! Thala owns its own durable store for tasks, runs, and interaction tickets.
//! This is separate from Beads (canonical task truth) and from the git repo.
//!
//! Implementation: SqliteStateStore (adapters/store/sqlite.rs).

use async_trait::async_trait;

use crate::core::error::ThalaError;
use crate::core::ids::{InteractionId, RunId, TaskId};
use crate::core::interaction::{InteractionResolution, InteractionTicket};
use crate::core::run::TaskRun;
use crate::core::task::TaskRecord;

// ── StateStore ────────────────────────────────────────────────────────────────

/// Durable storage for Thala's runtime state.
///
/// All write operations are upserts — calling upsert_task on an existing
/// task_id replaces the record.
#[async_trait]
pub trait StateStore: Send + Sync {
    // ── Task records ──────────────────────────────────────────────────────────

    /// Insert or update a task record.
    async fn upsert_task(&self, record: &TaskRecord) -> Result<(), ThalaError>;

    /// Fetch a task record by ID. Returns None if not found.
    async fn get_task(&self, task_id: &TaskId) -> Result<Option<TaskRecord>, ThalaError>;

    /// Fetch all task records in a non-terminal status.
    /// Used by the scheduler and reconciler.
    async fn active_tasks(&self) -> Result<Vec<TaskRecord>, ThalaError>;

    /// Fetch all task records (active and terminal).
    async fn all_tasks(&self) -> Result<Vec<TaskRecord>, ThalaError>;

    // ── Run records ───────────────────────────────────────────────────────────

    /// Insert or update a run record.
    async fn upsert_run(&self, run: &TaskRun) -> Result<(), ThalaError>;

    /// Fetch a run by ID.
    async fn get_run(&self, run_id: &RunId) -> Result<Option<TaskRun>, ThalaError>;

    /// Fetch all runs in a non-terminal status.
    /// Used by the monitor.
    async fn active_runs(&self) -> Result<Vec<TaskRun>, ThalaError>;

    /// Fetch all runs for a specific task (across all attempts).
    async fn runs_for_task(&self, task_id: &TaskId) -> Result<Vec<TaskRun>, ThalaError>;

    // ── Interaction tickets ───────────────────────────────────────────────────

    /// Persist a new interaction ticket.
    async fn save_ticket(&self, ticket: &InteractionTicket) -> Result<(), ThalaError>;

    /// Update an existing ticket (e.g. mark as sent, add channel message ref).
    async fn update_ticket(&self, ticket: &InteractionTicket) -> Result<(), ThalaError>;

    /// Fetch all unsent or pending tickets.
    async fn pending_tickets(&self) -> Result<Vec<InteractionTicket>, ThalaError>;

    /// Fetch a ticket by interaction request ID.
    async fn get_ticket(
        &self,
        interaction_id: &InteractionId,
    ) -> Result<Option<InteractionTicket>, ThalaError>;

    /// Record a resolution and mark the corresponding ticket as resolved.
    async fn resolve_ticket(&self, resolution: &InteractionResolution) -> Result<(), ThalaError>;
}
