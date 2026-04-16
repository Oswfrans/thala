//! State transition reducers for tasks and runs.
//!
//! All lifecycle changes must go through `apply_transition` or
//! `apply_run_transition`. Orchestration code must not mutate `.status`
//! fields directly.
//!
//! # Task lifecycle
//!
//!   Pending → Ready → Dispatching → Running
//!                                       ↓          ↓          ↓
//!                                 WaitingForHuman  Stuck    Validating
//!                                       ↓   ↑                   ↓      ↓
//!                                   Resolved Running        Succeeded  Failed
//!                                                              ↓
//!                                                           Resolved
//!
//! Terminal states (Succeeded, Failed, Resolved) can only be exited via
//! an explicit recovery Transition — and only if policy permits.
//!
//! # Run lifecycle
//!
//!   Launching → Active → Completed
//!                    ↓ → Failed
//!                    ↓ → TimedOut
//!                    ↓ → Cancelled

use chrono::Utc;

use crate::core::ids::RunId;
use crate::core::run::{RunStatus, TaskRun};
use crate::core::state::StateError;
use crate::core::task::{TaskRecord, TaskStatus};

// ── Task transitions ──────────────────────────────────────────────────────────

/// Every valid reason a task's status can change.
/// The orchestrator dispatches one of these; `apply_transition` validates
/// it against the current status and returns the updated record.
#[derive(Debug, Clone)]
pub enum Transition {
    /// Beads confirms the task is unblocked and ready for dispatch.
    MarkReady,

    /// The dispatcher has claimed this task and is building context.
    BeginDispatching,

    /// A run has been successfully launched.
    RunLaunched { run_id: RunId },

    /// The active run completed; move to validation.
    RunCompleted,

    /// The active run failed. If retries remain, this will be followed by
    /// a new Transition::BeginDispatching + RunLaunched sequence.
    RunFailed { reason: String },

    /// No progress detected within the stall timeout.
    RunStalled { reason: String },

    /// The orchestrator needs human input before continuing.
    /// Only valid from Running or Validating.
    RequireHumanInput,

    /// The human approved the action (merge, continue, etc.).
    HumanApproved,

    /// The human rejected the output — retry will follow.
    HumanRejected { reason: String },

    /// The human explicitly resolved/closed this task.
    HumanResolved,

    /// All validation passed — mark succeeded and write back to Beads.
    ValidationPassed,

    /// Validation failed but retries remain — dispatcher will re-launch.
    ValidationFailedRetry { reason: String },

    /// Validation failed with no retries remaining.
    ValidationFailedTerminal { reason: String },

    /// Human kicked a Stuck or Failed task back into the dispatch queue.
    /// This creates a new run; does not mutate the existing run.
    RecoveryRequested,
}

/// Attempt a task status transition. Returns a new `TaskRecord` on success.
///
/// This function is a pure reducer — it does not touch I/O.
/// Side effects (persisting the record, emitting events) are the caller's
/// responsibility.
pub fn apply_transition(
    record: &TaskRecord,
    transition: Transition,
) -> Result<TaskRecord, StateError> {
    let current = &record.status;
    let task_id = record.id().clone();

    #[allow(clippy::match_same_arms)]
    let next = match (&current, &transition) {
        // ── Pending ───────────────────────────────────────────────────────────
        (TaskStatus::Pending, Transition::MarkReady) => TaskStatus::Ready,

        // ── Ready ─────────────────────────────────────────────────────────────
        (TaskStatus::Ready, Transition::BeginDispatching) => TaskStatus::Dispatching,

        // ── Dispatching ───────────────────────────────────────────────────────
        (TaskStatus::Dispatching, Transition::RunLaunched { .. }) => TaskStatus::Running,

        // ── Running ───────────────────────────────────────────────────────────
        (TaskStatus::Running, Transition::RunCompleted) => TaskStatus::Validating,
        (TaskStatus::Running, Transition::RunFailed { .. }) => TaskStatus::Failed,
        (TaskStatus::Running, Transition::RunStalled { .. }) => TaskStatus::Stuck,
        (TaskStatus::Running, Transition::RequireHumanInput) => TaskStatus::WaitingForHuman,

        // ── WaitingForHuman ───────────────────────────────────────────────────
        (TaskStatus::WaitingForHuman, Transition::HumanApproved) => TaskStatus::Validating,
        (TaskStatus::WaitingForHuman, Transition::HumanRejected { .. }) => TaskStatus::Dispatching,
        (TaskStatus::WaitingForHuman, Transition::HumanResolved) => TaskStatus::Resolved,
        (TaskStatus::WaitingForHuman, Transition::RecoveryRequested) => TaskStatus::Dispatching,

        // ── Validating ────────────────────────────────────────────────────────
        (TaskStatus::Validating, Transition::ValidationPassed) => TaskStatus::Succeeded,
        (TaskStatus::Validating, Transition::ValidationFailedRetry { .. }) => {
            TaskStatus::Dispatching
        }
        (TaskStatus::Validating, Transition::ValidationFailedTerminal { .. }) => TaskStatus::Failed,
        (TaskStatus::Validating, Transition::RequireHumanInput) => TaskStatus::WaitingForHuman,

        // ── Stuck ─────────────────────────────────────────────────────────────
        (TaskStatus::Stuck, Transition::RecoveryRequested) => TaskStatus::Dispatching,
        (TaskStatus::Stuck, Transition::HumanResolved) => TaskStatus::Resolved,

        // ── Failed ────────────────────────────────────────────────────────────
        // Recovery requires explicit human action; not an automatic transition.
        (TaskStatus::Failed, Transition::RecoveryRequested) => TaskStatus::Dispatching,
        (TaskStatus::Failed, Transition::HumanResolved) => TaskStatus::Resolved,

        // ── Terminal guard ────────────────────────────────────────────────────
        (s, _) if s.is_terminal() => {
            return Err(StateError::TaskTerminal(task_id));
        }

        // ── Illegal ───────────────────────────────────────────────────────────
        _ => {
            return Err(StateError::IllegalTaskTransition {
                task_id,
                from: current.clone(),
                to: transition_target_status(&transition),
                reason: format!("{transition:?}"),
            });
        }
    };

    let mut updated = record.clone();
    updated.status = next;

    // Set or clear active_run_id based on the transition.
    match &transition {
        Transition::RunLaunched { run_id } => {
            updated.active_run_id = Some(run_id.clone());
            updated.attempt += 1;
        }
        Transition::ValidationPassed
        | Transition::HumanResolved
        | Transition::ValidationFailedTerminal { .. } => {
            updated.active_run_id = None;
        }
        _ => {}
    }

    updated.touch();

    tracing::info!(
        task_id = %task_id,
        from = %current.as_str(),
        to = %updated.status.as_str(),
        "Task state transition"
    );

    Ok(updated)
}

// ── Run transitions ───────────────────────────────────────────────────────────

/// Every valid reason a run's status can change.
#[derive(Debug, Clone)]
pub enum RunTransition {
    /// The backend confirmed the worker is running (spawned successfully).
    Activated,

    /// The monitor observed a change in worker output.
    ActivityObserved { cursor: String },

    /// The worker signaled successful completion (signal file or callback).
    CompletionSignaled,

    /// The worker reported failure (non-zero exit or failure callback).
    FailureSignaled { reason: String },

    /// No progress detected within the stall timeout.
    StallTimeout,

    /// The orchestrator forcefully terminated the worker.
    Cancelled,
}

/// Attempt a run status transition. Returns a new `TaskRun` on success.
///
/// This function is a pure reducer — no I/O.
pub fn apply_run_transition(
    run: &TaskRun,
    transition: RunTransition,
) -> Result<TaskRun, StateError> {
    let current = &run.status;
    let run_id = run.run_id.clone();

    #[allow(clippy::match_same_arms)]
    let next = match (&current, &transition) {
        (RunStatus::Launching, RunTransition::Activated) => RunStatus::Active,
        (RunStatus::Launching, RunTransition::Cancelled) => RunStatus::Cancelled,

        (RunStatus::Active, RunTransition::CompletionSignaled) => RunStatus::Completed,
        (RunStatus::Active, RunTransition::FailureSignaled { .. }) => RunStatus::Failed,
        (RunStatus::Active, RunTransition::StallTimeout) => RunStatus::TimedOut,
        (RunStatus::Active, RunTransition::Cancelled) => RunStatus::Cancelled,
        // ActivityObserved does not change status; it updates the cursor.
        (RunStatus::Active, RunTransition::ActivityObserved { .. }) => RunStatus::Active,

        (s, _) if s.is_terminal() => {
            return Err(StateError::RunTerminal(run_id));
        }

        _ => {
            return Err(StateError::IllegalRunTransition {
                run_id,
                from: current.clone(),
                to: run_transition_target_status(&transition),
                reason: format!("{transition:?}"),
            });
        }
    };

    let mut updated = run.clone();
    updated.status = next;

    // Apply side-effects of specific transitions.
    match &transition {
        RunTransition::ActivityObserved { cursor } => {
            updated.last_observation_cursor = Some(cursor.clone());
            updated.last_activity_at = Some(Utc::now());
        }
        RunTransition::CompletionSignaled
        | RunTransition::FailureSignaled { .. }
        | RunTransition::StallTimeout
        | RunTransition::Cancelled => {
            updated.mark_completed();
        }
        RunTransition::Activated => {}
    }

    updated.touch();

    tracing::info!(
        run_id = %run.run_id,
        task_id = %run.task_id,
        from = %current.as_str(),
        to = %updated.status.as_str(),
        "Run state transition"
    );

    Ok(updated)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Approximate the target status from a Transition for error messages.
/// Not used for routing — only for constructing descriptive errors.
fn transition_target_status(t: &Transition) -> TaskStatus {
    match t {
        Transition::MarkReady => TaskStatus::Ready,
        Transition::BeginDispatching
        | Transition::ValidationFailedRetry { .. }
        | Transition::HumanRejected { .. }
        | Transition::RecoveryRequested => TaskStatus::Dispatching,
        Transition::RunLaunched { .. } => TaskStatus::Running,
        Transition::RunCompleted | Transition::HumanApproved => TaskStatus::Validating,
        Transition::RunFailed { .. } | Transition::ValidationFailedTerminal { .. } => {
            TaskStatus::Failed
        }
        Transition::RunStalled { .. } => TaskStatus::Stuck,
        Transition::RequireHumanInput => TaskStatus::WaitingForHuman,
        Transition::ValidationPassed => TaskStatus::Succeeded,
        Transition::HumanResolved => TaskStatus::Resolved,
    }
}

fn run_transition_target_status(t: &RunTransition) -> RunStatus {
    match t {
        RunTransition::Activated | RunTransition::ActivityObserved { .. } => RunStatus::Active,
        RunTransition::CompletionSignaled => RunStatus::Completed,
        RunTransition::FailureSignaled { .. } => RunStatus::Failed,
        RunTransition::StallTimeout => RunStatus::TimedOut,
        RunTransition::Cancelled => RunStatus::Cancelled,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ids::TaskId;
    use crate::core::task::{TaskRecord, TaskSpec};

    fn make_spec(id: &str) -> TaskSpec {
        TaskSpec {
            id: TaskId::new(id),
            title: "Test task".into(),
            acceptance_criteria: "It works".into(),
            context: String::new(),
            beads_ref: id.into(),
            model_override: None,
            always_human_review: false,
            labels: vec![],
        }
    }

    fn pending_record(id: &str) -> TaskRecord {
        TaskRecord::new(make_spec(id))
    }

    #[test]
    fn pending_to_ready_is_legal() {
        let rec = pending_record("bd-0001");
        let updated = apply_transition(&rec, Transition::MarkReady).unwrap();
        assert_eq!(updated.status, TaskStatus::Ready);
    }

    #[test]
    fn pending_to_dispatching_is_illegal() {
        let rec = pending_record("bd-0002");
        let err = apply_transition(&rec, Transition::BeginDispatching).unwrap_err();
        assert!(matches!(err, StateError::IllegalTaskTransition { .. }));
    }

    #[test]
    fn full_happy_path_compiles() {
        let run_id = RunId::new_v4();
        let rec = pending_record("bd-0003");
        let rec = apply_transition(&rec, Transition::MarkReady).unwrap();
        let rec = apply_transition(&rec, Transition::BeginDispatching).unwrap();
        let rec = apply_transition(
            &rec,
            Transition::RunLaunched {
                run_id: run_id.clone(),
            },
        )
        .unwrap();
        assert_eq!(rec.status, TaskStatus::Running);
        assert_eq!(rec.active_run_id, Some(run_id));
        assert_eq!(rec.attempt, 1);
        let rec = apply_transition(&rec, Transition::RunCompleted).unwrap();
        assert_eq!(rec.status, TaskStatus::Validating);
        let rec = apply_transition(&rec, Transition::ValidationPassed).unwrap();
        assert_eq!(rec.status, TaskStatus::Succeeded);
        assert!(rec.status.is_terminal());
    }

    #[test]
    fn terminal_task_rejects_all_transitions() {
        let mut rec = pending_record("bd-0004");
        rec.status = TaskStatus::Succeeded;
        let err = apply_transition(&rec, Transition::MarkReady).unwrap_err();
        assert!(matches!(err, StateError::TaskTerminal(_)));
    }

    #[test]
    fn run_launching_to_active() {
        let run = TaskRun::new(
            RunId::new_v4(),
            TaskId::new("bd-0005"),
            1,
            crate::core::run::ExecutionBackendKind::Local,
        );
        let updated = apply_run_transition(&run, RunTransition::Activated).unwrap();
        assert_eq!(updated.status, RunStatus::Active);
    }

    #[test]
    fn run_terminal_rejects_transitions() {
        let mut run = TaskRun::new(
            RunId::new_v4(),
            TaskId::new("bd-0006"),
            1,
            crate::core::run::ExecutionBackendKind::Local,
        );
        run.status = RunStatus::Completed;
        let err = apply_run_transition(&run, RunTransition::Activated).unwrap_err();
        assert!(matches!(err, StateError::RunTerminal(_)));
    }

    #[test]
    fn activity_observed_updates_cursor_without_status_change() {
        let mut run = TaskRun::new(
            RunId::new_v4(),
            TaskId::new("bd-0007"),
            1,
            crate::core::run::ExecutionBackendKind::Local,
        );
        run.status = RunStatus::Active;
        let updated = apply_run_transition(
            &run,
            RunTransition::ActivityObserved {
                cursor: "abc123".into(),
            },
        )
        .unwrap();
        assert_eq!(updated.status, RunStatus::Active);
        assert_eq!(updated.last_observation_cursor.as_deref(), Some("abc123"));
    }
}
