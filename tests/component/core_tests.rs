//! Core domain tests — state machine transitions and pure logic.
//!
//! These tests verify the transition rules and domain invariants without I/O.

use thala::core::ids::{RunId, TaskId};
use thala::core::run::{ExecutionBackendKind, RunStatus, TaskRun};
use thala::core::state::StateError;
use thala::core::task::{TaskRecord, TaskSpec, TaskStatus};
use thala::core::transitions::{apply_run_transition, apply_transition, RunTransition, Transition};

// ─────────────────────────────────────────────────────────────────────────────
// Test helpers
// ─────────────────────────────────────────────────────────────────────────────

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

fn make_run(task_id: &str, attempt: u32) -> TaskRun {
    TaskRun::new(
        RunId::new_v4(),
        TaskId::new(task_id),
        attempt,
        ExecutionBackendKind::Local,
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Task state transition tests
// ─────────────────────────────────────────────────────────────────────────────

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
fn ready_to_dispatching_is_legal() {
    let mut rec = pending_record("bd-0003");
    rec.status = TaskStatus::Ready;
    let updated = apply_transition(&rec, Transition::BeginDispatching).unwrap();
    assert_eq!(updated.status, TaskStatus::Dispatching);
}

#[test]
fn dispatching_to_running_is_legal() {
    let mut rec = pending_record("bd-0004");
    rec.status = TaskStatus::Dispatching;
    let run_id = RunId::new_v4();
    let updated = apply_transition(
        &rec,
        Transition::RunLaunched {
            run_id: run_id.clone(),
        },
    )
    .unwrap();
    assert_eq!(updated.status, TaskStatus::Running);
    assert_eq!(updated.active_run_id, Some(run_id));
    assert_eq!(updated.attempt, 1);
}

#[test]
fn running_to_validating_is_legal() {
    let mut rec = pending_record("bd-0005");
    rec.status = TaskStatus::Running;
    let updated = apply_transition(&rec, Transition::RunCompleted).unwrap();
    assert_eq!(updated.status, TaskStatus::Validating);
}

#[test]
fn running_to_failed_is_legal() {
    let mut rec = pending_record("bd-0006");
    rec.status = TaskStatus::Running;
    let updated = apply_transition(
        &rec,
        Transition::RunFailed {
            reason: "crash".into(),
        },
    )
    .unwrap();
    assert_eq!(updated.status, TaskStatus::Failed);
}

#[test]
fn running_to_stuck_is_legal() {
    let mut rec = pending_record("bd-0007");
    rec.status = TaskStatus::Running;
    let updated = apply_transition(
        &rec,
        Transition::RunStalled {
            reason: "timeout".into(),
        },
    )
    .unwrap();
    assert_eq!(updated.status, TaskStatus::Stuck);
}

#[test]
fn running_to_waiting_for_human_is_legal() {
    let mut rec = pending_record("bd-0008");
    rec.status = TaskStatus::Running;
    let updated = apply_transition(&rec, Transition::RequireHumanInput).unwrap();
    assert_eq!(updated.status, TaskStatus::WaitingForHuman);
}

#[test]
fn validating_to_succeeded_is_legal() {
    let mut rec = pending_record("bd-0009");
    rec.status = TaskStatus::Validating;
    let updated = apply_transition(&rec, Transition::ValidationPassed).unwrap();
    assert_eq!(updated.status, TaskStatus::Succeeded);
    assert!(updated.active_run_id.is_none());
}

#[test]
fn validating_to_failed_terminal_is_legal() {
    let mut rec = pending_record("bd-0010");
    rec.status = TaskStatus::Validating;
    let updated = apply_transition(
        &rec,
        Transition::ValidationFailedTerminal {
            reason: "bad PR".into(),
        },
    )
    .unwrap();
    assert_eq!(updated.status, TaskStatus::Failed);
}

#[test]
fn validating_to_dispatching_via_retry_is_legal() {
    let mut rec = pending_record("bd-0011");
    rec.status = TaskStatus::Validating;
    let updated = apply_transition(
        &rec,
        Transition::ValidationFailedRetry {
            reason: "needs work".into(),
        },
    )
    .unwrap();
    assert_eq!(updated.status, TaskStatus::Dispatching);
}

#[test]
fn validating_to_waiting_for_human_is_legal() {
    let mut rec = pending_record("bd-0012");
    rec.status = TaskStatus::Validating;
    let updated = apply_transition(&rec, Transition::RequireHumanInput).unwrap();
    assert_eq!(updated.status, TaskStatus::WaitingForHuman);
}

#[test]
fn waiting_for_human_to_validating_is_legal() {
    let mut rec = pending_record("bd-0013");
    rec.status = TaskStatus::WaitingForHuman;
    let updated = apply_transition(&rec, Transition::HumanApproved).unwrap();
    assert_eq!(updated.status, TaskStatus::Validating);
}

#[test]
fn waiting_for_human_to_dispatching_via_rejection_is_legal() {
    let mut rec = pending_record("bd-0014");
    rec.status = TaskStatus::WaitingForHuman;
    let updated = apply_transition(
        &rec,
        Transition::HumanRejected {
            reason: "redo".into(),
        },
    )
    .unwrap();
    assert_eq!(updated.status, TaskStatus::Dispatching);
}

#[test]
fn waiting_for_human_to_resolved_is_legal() {
    let mut rec = pending_record("bd-0015");
    rec.status = TaskStatus::WaitingForHuman;
    let updated = apply_transition(&rec, Transition::HumanResolved).unwrap();
    assert_eq!(updated.status, TaskStatus::Resolved);
    assert!(updated.active_run_id.is_none());
}

#[test]
fn waiting_for_human_to_dispatching_via_recovery_is_legal() {
    let mut rec = pending_record("bd-0016");
    rec.status = TaskStatus::WaitingForHuman;
    let updated = apply_transition(&rec, Transition::RecoveryRequested).unwrap();
    assert_eq!(updated.status, TaskStatus::Dispatching);
}

#[test]
fn stuck_to_dispatching_via_recovery_is_legal() {
    let mut rec = pending_record("bd-0017");
    rec.status = TaskStatus::Stuck;
    let updated = apply_transition(&rec, Transition::RecoveryRequested).unwrap();
    assert_eq!(updated.status, TaskStatus::Dispatching);
}

#[test]
fn stuck_to_resolved_is_legal() {
    let mut rec = pending_record("bd-0018");
    rec.status = TaskStatus::Stuck;
    let updated = apply_transition(&rec, Transition::HumanResolved).unwrap();
    assert_eq!(updated.status, TaskStatus::Resolved);
}

#[test]
fn failed_to_dispatching_via_recovery_is_legal() {
    let mut rec = pending_record("bd-0019");
    rec.status = TaskStatus::Failed;
    let updated = apply_transition(&rec, Transition::RecoveryRequested).unwrap();
    assert_eq!(updated.status, TaskStatus::Dispatching);
}

#[test]
fn failed_to_resolved_is_legal() {
    let mut rec = pending_record("bd-0020");
    rec.status = TaskStatus::Failed;
    let updated = apply_transition(&rec, Transition::HumanResolved).unwrap();
    assert_eq!(updated.status, TaskStatus::Resolved);
}

#[test]
fn terminal_task_rejects_all_transitions() {
    let mut rec = pending_record("bd-0021");
    rec.status = TaskStatus::Succeeded;
    let err = apply_transition(&rec, Transition::MarkReady).unwrap_err();
    assert!(matches!(err, StateError::TaskTerminal(_)));

    let mut rec = pending_record("bd-0022");
    rec.status = TaskStatus::Failed;
    let err = apply_transition(&rec, Transition::BeginDispatching).unwrap_err();
    assert!(matches!(err, StateError::TaskTerminal(_)));

    let mut rec = pending_record("bd-0023");
    rec.status = TaskStatus::Resolved;
    let err = apply_transition(&rec, Transition::RunCompleted).unwrap_err();
    assert!(matches!(err, StateError::TaskTerminal(_)));
}

#[test]
fn full_happy_path_compiles() {
    let run_id = RunId::new_v4();
    let rec = pending_record("bd-0024");
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
fn full_retry_path_compiles() {
    let run_id1 = RunId::new_v4();
    let run_id2 = RunId::new_v4();

    let rec = pending_record("bd-0025");
    let rec = apply_transition(&rec, Transition::MarkReady).unwrap();
    let rec = apply_transition(&rec, Transition::BeginDispatching).unwrap();
    let rec = apply_transition(
        &rec,
        Transition::RunLaunched {
            run_id: run_id1.clone(),
        },
    )
    .unwrap();
    assert_eq!(rec.attempt, 1);

    // Run completes
    let rec = apply_transition(&rec, Transition::RunCompleted).unwrap();
    assert_eq!(rec.status, TaskStatus::Validating);

    // First attempt fails validation, retry requested
    let rec = apply_transition(
        &rec,
        Transition::ValidationFailedRetry {
            reason: "needs fix".into(),
        },
    )
    .unwrap();
    assert_eq!(rec.status, TaskStatus::Dispatching);
    // Note: active_run_id is not cleared on retry - the old run stays associated until a new run launches

    // Second attempt launches
    let rec = apply_transition(
        &rec,
        Transition::RunLaunched {
            run_id: run_id2.clone(),
        },
    )
    .unwrap();
    assert_eq!(rec.attempt, 2);
    assert_eq!(rec.active_run_id, Some(run_id2));

    // Second attempt succeeds
    let rec = apply_transition(&rec, Transition::RunCompleted).unwrap();
    let rec = apply_transition(&rec, Transition::ValidationPassed).unwrap();
    assert_eq!(rec.status, TaskStatus::Succeeded);
}

// ─────────────────────────────────────────────────────────────────────────────
// Run state transition tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn run_launching_to_active_is_legal() {
    let run = make_run("bd-0100", 1);
    assert_eq!(run.status, RunStatus::Launching);
    let updated = apply_run_transition(&run, RunTransition::Activated).unwrap();
    assert_eq!(updated.status, RunStatus::Active);
}

#[test]
fn run_launching_to_cancelled_is_legal() {
    let run = make_run("bd-0101", 1);
    let updated = apply_run_transition(&run, RunTransition::Cancelled).unwrap();
    assert_eq!(updated.status, RunStatus::Cancelled);
    assert!(updated.completed_at.is_some());
}

#[test]
fn run_active_to_completed_is_legal() {
    let mut run = make_run("bd-0102", 1);
    run.status = RunStatus::Active;
    let updated = apply_run_transition(&run, RunTransition::CompletionSignaled).unwrap();
    assert_eq!(updated.status, RunStatus::Completed);
    assert!(updated.completed_at.is_some());
}

#[test]
fn run_active_to_failed_is_legal() {
    let mut run = make_run("bd-0103", 1);
    run.status = RunStatus::Active;
    let updated = apply_run_transition(
        &run,
        RunTransition::FailureSignaled {
            reason: "exit 1".into(),
        },
    )
    .unwrap();
    assert_eq!(updated.status, RunStatus::Failed);
}

#[test]
fn run_active_to_timed_out_is_legal() {
    let mut run = make_run("bd-0104", 1);
    run.status = RunStatus::Active;
    let updated = apply_run_transition(&run, RunTransition::StallTimeout).unwrap();
    assert_eq!(updated.status, RunStatus::TimedOut);
}

#[test]
fn run_active_to_cancelled_is_legal() {
    let mut run = make_run("bd-0105", 1);
    run.status = RunStatus::Active;
    let updated = apply_run_transition(&run, RunTransition::Cancelled).unwrap();
    assert_eq!(updated.status, RunStatus::Cancelled);
}

#[test]
fn run_active_activity_observed_preserves_status() {
    let mut run = make_run("bd-0106", 1);
    run.status = RunStatus::Active;
    let updated = apply_run_transition(
        &run,
        RunTransition::ActivityObserved {
            cursor: "abc123".into(),
        },
    )
    .unwrap();
    assert_eq!(updated.status, RunStatus::Active);
    assert_eq!(updated.last_observation_cursor, Some("abc123".into()));
    assert!(updated.last_activity_at.is_some());
}

#[test]
fn run_terminal_rejects_transitions() {
    let mut run = make_run("bd-0107", 1);
    run.status = RunStatus::Completed;
    let err = apply_run_transition(&run, RunTransition::Activated).unwrap_err();
    assert!(matches!(err, StateError::RunTerminal(_)));

    let mut run = make_run("bd-0108", 1);
    run.status = RunStatus::Failed;
    let err = apply_run_transition(&run, RunTransition::CompletionSignaled).unwrap_err();
    assert!(matches!(err, StateError::RunTerminal(_)));

    let mut run = make_run("bd-0109", 1);
    run.status = RunStatus::Cancelled;
    let err = apply_run_transition(&run, RunTransition::Activated).unwrap_err();
    assert!(matches!(err, StateError::RunTerminal(_)));

    let mut run = make_run("bd-0110", 1);
    run.status = RunStatus::TimedOut;
    let err = apply_run_transition(&run, RunTransition::Activated).unwrap_err();
    assert!(matches!(err, StateError::RunTerminal(_)));
}

#[test]
fn run_illegal_transitions_are_rejected() {
    // Launching cannot go directly to Completed
    let run = make_run("bd-0111", 1);
    let err = apply_run_transition(&run, RunTransition::CompletionSignaled).unwrap_err();
    assert!(matches!(err, StateError::IllegalRunTransition { .. }));

    // Launching cannot go directly to Failed
    let run = make_run("bd-0112", 1);
    let err = apply_run_transition(
        &run,
        RunTransition::FailureSignaled {
            reason: "crash".into(),
        },
    )
    .unwrap_err();
    assert!(matches!(err, StateError::IllegalRunTransition { .. }));
}

// ─────────────────────────────────────────────────────────────────────────────
// TaskSpec dispatchability tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn task_with_acceptance_criteria_is_dispatchable() {
    let spec = make_spec("bd-0200");
    assert!(spec.is_dispatchable());
}

#[test]
fn task_without_acceptance_criteria_is_not_dispatchable() {
    let mut spec = make_spec("bd-0201");
    spec.acceptance_criteria = "".into();
    assert!(!spec.is_dispatchable());
}

#[test]
fn task_with_whitespace_only_ac_is_not_dispatchable() {
    let mut spec = make_spec("bd-0202");
    spec.acceptance_criteria = "   \n\t  ".into();
    assert!(!spec.is_dispatchable());
}
