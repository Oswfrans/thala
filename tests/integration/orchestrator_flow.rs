//! Orchestrator flow integration tests.
//!
//! Tests multi-component interactions: scheduler → dispatcher → monitor.

use thala::adapters::state::SqliteStateStore;
use thala::core::ids::{RunId, TaskId};
use thala::core::run::{ExecutionBackendKind, RunStatus};
use thala::core::task::{TaskRecord, TaskSpec, TaskStatus};
use thala::core::transitions::{apply_run_transition, apply_transition, RunTransition, Transition};
use thala::ports::state_store::StateStore;

// ─────────────────────────────────────────────────────────────────────────────
// End-to-end task lifecycle tests
// ─────────────────────────────────────────────────────────────────────────────

fn make_spec(id: &str) -> TaskSpec {
    TaskSpec {
        id: TaskId::new(id),
        title: "Integration test task".into(),
        acceptance_criteria: "It works".into(),
        context: String::new(),
        beads_ref: id.into(),
        model_override: None,
        always_human_review: false,
        labels: vec![],
    }
}

#[tokio::test]
async fn full_task_lifecycle_with_state_store() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    let store = SqliteStateStore::open(&db_path).unwrap();

    // 1. Scheduler ingests a new task from Beads
    let spec = make_spec("bd-int-001");
    let rec = TaskRecord::new(spec);
    store.upsert_task(&rec).await.unwrap();

    // 2. Scheduler marks it ready
    let rec = apply_transition(&rec, Transition::MarkReady).unwrap();
    store.upsert_task(&rec).await.unwrap();
    assert_eq!(rec.status, TaskStatus::Ready);

    // 3. Dispatcher begins dispatching
    let rec = apply_transition(&rec, Transition::BeginDispatching).unwrap();
    store.upsert_task(&rec).await.unwrap();
    assert_eq!(rec.status, TaskStatus::Dispatching);

    // 4. Run is launched
    let run_id = RunId::new_v4();
    let rec = apply_transition(
        &rec,
        Transition::RunLaunched {
            run_id: run_id.clone(),
        },
    )
    .unwrap();
    store.upsert_task(&rec).await.unwrap();
    assert_eq!(rec.status, TaskStatus::Running);
    assert_eq!(rec.active_run_id, Some(run_id.clone()));

    // 5. Create the run record
    let run = thala::core::run::TaskRun::new(
        run_id.clone(),
        TaskId::new("bd-int-001"),
        1,
        ExecutionBackendKind::Local,
    );
    store.upsert_run(&run).await.unwrap();

    // 6. Monitor observes activation
    let run = apply_run_transition(&run, RunTransition::Activated).unwrap();
    store.upsert_run(&run).await.unwrap();
    assert_eq!(run.status, RunStatus::Active);

    // 7. Run completes successfully
    let run = apply_run_transition(&run, RunTransition::CompletionSignaled).unwrap();
    store.upsert_run(&run).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);

    // 8. Task moves to validating
    let rec = apply_transition(&rec, Transition::RunCompleted).unwrap();
    store.upsert_task(&rec).await.unwrap();
    assert_eq!(rec.status, TaskStatus::Validating);

    // 9. Validation passes
    let rec = apply_transition(&rec, Transition::ValidationPassed).unwrap();
    store.upsert_task(&rec).await.unwrap();
    assert_eq!(rec.status, TaskStatus::Succeeded);
    assert!(rec.status.is_terminal());
    assert!(rec.active_run_id.is_none());

    // 10. Verify final state in store
    let final_task = store
        .get_task(&TaskId::new("bd-int-001"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(final_task.status, TaskStatus::Succeeded);
    assert_eq!(final_task.attempt, 1);

    let final_run = store.get_run(&run_id).await.unwrap().unwrap();
    assert_eq!(final_run.status, RunStatus::Completed);
}

#[tokio::test]
async fn retry_flow_with_multiple_attempts() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    let store = SqliteStateStore::open(&db_path).unwrap();

    let spec = make_spec("bd-int-002");
    let rec = TaskRecord::new(spec);

    // Attempt 1: Fails validation, requests retry
    let run_id1 = RunId::new_v4();
    let rec = apply_transition(&rec, Transition::MarkReady).unwrap();
    let rec = apply_transition(&rec, Transition::BeginDispatching).unwrap();
    let rec = apply_transition(
        &rec,
        Transition::RunLaunched {
            run_id: run_id1.clone(),
        },
    )
    .unwrap();

    let run1 = thala::core::run::TaskRun::new(
        run_id1.clone(),
        TaskId::new("bd-int-002"),
        1,
        ExecutionBackendKind::Local,
    );

    // Simulate run completing but validation failing
    let rec = apply_transition(&rec, Transition::RunCompleted).unwrap();
    let rec = apply_transition(
        &rec,
        Transition::ValidationFailedRetry {
            reason: "needs fix".into(),
        },
    )
    .unwrap();
    assert_eq!(rec.status, TaskStatus::Dispatching);
    assert_eq!(rec.attempt, 1); // Still 1, hasn't launched new run yet

    // Attempt 2: Succeeds
    let run_id2 = RunId::new_v4();
    let rec = apply_transition(
        &rec,
        Transition::RunLaunched {
            run_id: run_id2.clone(),
        },
    )
    .unwrap();
    assert_eq!(rec.attempt, 2);

    let run2 = thala::core::run::TaskRun::new(
        run_id2.clone(),
        TaskId::new("bd-int-002"),
        2,
        ExecutionBackendKind::Local,
    );

    // Complete and validate
    let rec = apply_transition(&rec, Transition::RunCompleted).unwrap();
    let rec = apply_transition(&rec, Transition::ValidationPassed).unwrap();
    assert_eq!(rec.status, TaskStatus::Succeeded);

    // Store both runs
    store.upsert_run(&run1).await.unwrap();
    store.upsert_run(&run2).await.unwrap();
    store.upsert_task(&rec).await.unwrap();

    // Verify both runs are stored
    let runs = store
        .runs_for_task(&TaskId::new("bd-int-002"))
        .await
        .unwrap();
    assert_eq!(runs.len(), 2);
}

#[tokio::test]
async fn stall_detection_flow() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    let store = SqliteStateStore::open(&db_path).unwrap();

    let spec = make_spec("bd-int-003");
    let rec = TaskRecord::new(spec);

    let run_id = RunId::new_v4();
    let rec = apply_transition(&rec, Transition::MarkReady).unwrap();
    let rec = apply_transition(&rec, Transition::BeginDispatching).unwrap();
    let rec = apply_transition(
        &rec,
        Transition::RunLaunched {
            run_id: run_id.clone(),
        },
    )
    .unwrap();

    let mut run = thala::core::run::TaskRun::new(
        run_id.clone(),
        TaskId::new("bd-int-003"),
        1,
        ExecutionBackendKind::Local,
    );
    run.status = thala::core::run::RunStatus::Active;

    // Simulate stall detected by monitor
    let rec = apply_transition(
        &rec,
        Transition::RunStalled {
            reason: "no output for 5m".into(),
        },
    )
    .unwrap();
    assert_eq!(rec.status, TaskStatus::Stuck);

    // Human intervenes and requests recovery
    let rec = apply_transition(&rec, Transition::RecoveryRequested).unwrap();
    assert_eq!(rec.status, TaskStatus::Dispatching);

    // Save to store
    store.upsert_task(&rec).await.unwrap();

    let task = store
        .get_task(&TaskId::new("bd-int-003"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(task.status, TaskStatus::Dispatching);
}

#[tokio::test]
async fn human_approval_flow() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    let store = SqliteStateStore::open(&db_path).unwrap();

    let mut spec = make_spec("bd-int-004");
    spec.always_human_review = true; // Task requires human review
    let rec = TaskRecord::new(spec);

    let run_id = RunId::new_v4();
    let rec = apply_transition(&rec, Transition::MarkReady).unwrap();
    let rec = apply_transition(&rec, Transition::BeginDispatching).unwrap();
    let rec = apply_transition(
        &rec,
        Transition::RunLaunched {
            run_id: run_id.clone(),
        },
    )
    .unwrap();
    let rec = apply_transition(&rec, Transition::RunCompleted).unwrap();

    // Task needs human approval before final validation
    let rec = apply_transition(&rec, Transition::RequireHumanInput).unwrap();
    assert_eq!(rec.status, TaskStatus::WaitingForHuman);

    // Human approves
    let rec = apply_transition(&rec, Transition::HumanApproved).unwrap();
    assert_eq!(rec.status, TaskStatus::Validating);

    // Now validation passes
    let rec = apply_transition(&rec, Transition::ValidationPassed).unwrap();
    assert_eq!(rec.status, TaskStatus::Succeeded);

    store.upsert_task(&rec).await.unwrap();
}

#[tokio::test]
async fn human_rejection_triggers_retry() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    let store = SqliteStateStore::open(&db_path).unwrap();

    let spec = make_spec("bd-int-005");
    let rec = TaskRecord::new(spec);

    let run_id = RunId::new_v4();
    let rec = apply_transition(&rec, Transition::MarkReady).unwrap();
    let rec = apply_transition(&rec, Transition::BeginDispatching).unwrap();
    let rec = apply_transition(
        &rec,
        Transition::RunLaunched {
            run_id: run_id.clone(),
        },
    )
    .unwrap();
    let rec = apply_transition(&rec, Transition::RunCompleted).unwrap();

    // Human review requested
    let rec = apply_transition(&rec, Transition::RequireHumanInput).unwrap();
    assert_eq!(rec.status, TaskStatus::WaitingForHuman);

    // Human rejects with feedback
    let rec = apply_transition(
        &rec,
        Transition::HumanRejected {
            reason: "fix the alignment".into(),
        },
    )
    .unwrap();
    assert_eq!(rec.status, TaskStatus::Dispatching);

    // Ready for new attempt (active_run_id not cleared until new run launches)

    store.upsert_task(&rec).await.unwrap();
}
