//! Port implementation tests — StateStore, Validator, and other ports.

use thala::adapters::state::SqliteStateStore;
use thala::core::ids::{RunId, TaskId};
use thala::core::interaction::{
    InteractionAction, InteractionRequest, InteractionRequestKind, InteractionResolution,
    InteractionTicket,
};
use thala::core::run::{ExecutionBackendKind, RunStatus, TaskRun};
use thala::core::task::{TaskRecord, TaskSpec, TaskStatus};
use thala::ports::state_store::StateStore;

// ─────────────────────────────────────────────────────────────────────────────
// SqliteStateStore tests
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

fn make_record(id: &str, status: TaskStatus) -> TaskRecord {
    let mut rec = TaskRecord::new(make_spec(id));
    rec.status = status;
    rec
}

fn make_run(task_id: &str, attempt: u32, status: RunStatus) -> TaskRun {
    let mut run = TaskRun::new(
        RunId::new_v4(),
        TaskId::new(task_id),
        attempt,
        ExecutionBackendKind::Local,
    );
    run.status = status;
    run
}

#[tokio::test]
async fn sqlite_store_upsert_and_get_task() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    let store = SqliteStateStore::open(&db_path).unwrap();

    let rec = make_record("bd-1000", TaskStatus::Pending);
    store.upsert_task(&rec).await.unwrap();

    let fetched = store.get_task(&TaskId::new("bd-1000")).await.unwrap();
    assert!(fetched.is_some());
    let fetched = fetched.unwrap();
    assert_eq!(fetched.spec.id.as_str(), "bd-1000");
    assert_eq!(fetched.status, TaskStatus::Pending);
    assert_eq!(fetched.attempt, 0);
}

#[tokio::test]
async fn sqlite_store_upsert_updates_existing_task() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    let store = SqliteStateStore::open(&db_path).unwrap();

    let mut rec = make_record("bd-1001", TaskStatus::Pending);
    store.upsert_task(&rec).await.unwrap();

    // Update and save again
    rec.status = TaskStatus::Running;
    rec.attempt = 1;
    store.upsert_task(&rec).await.unwrap();

    let fetched = store
        .get_task(&TaskId::new("bd-1001"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fetched.status, TaskStatus::Running);
    assert_eq!(fetched.attempt, 1);
}

#[tokio::test]
async fn sqlite_store_get_task_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    let store = SqliteStateStore::open(&db_path).unwrap();

    let fetched = store.get_task(&TaskId::new("nonexistent")).await.unwrap();
    assert!(fetched.is_none());
}

#[tokio::test]
async fn sqlite_store_all_tasks_returns_all() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    let store = SqliteStateStore::open(&db_path).unwrap();

    let rec1 = make_record("bd-1002", TaskStatus::Pending);
    let rec2 = make_record("bd-1003", TaskStatus::Running);
    store.upsert_task(&rec1).await.unwrap();
    store.upsert_task(&rec2).await.unwrap();

    let all = store.all_tasks().await.unwrap();
    assert_eq!(all.len(), 2);
}

#[tokio::test]
async fn sqlite_store_active_tasks_filters_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    let store = SqliteStateStore::open(&db_path).unwrap();

    let rec1 = make_record("bd-1004", TaskStatus::Pending); // non-terminal
    let rec2 = make_record("bd-1005", TaskStatus::Succeeded); // terminal
    let rec3 = make_record("bd-1006", TaskStatus::Failed); // terminal
    let rec4 = make_record("bd-1007", TaskStatus::Running); // non-terminal

    store.upsert_task(&rec1).await.unwrap();
    store.upsert_task(&rec2).await.unwrap();
    store.upsert_task(&rec3).await.unwrap();
    store.upsert_task(&rec4).await.unwrap();

    let active = store.active_tasks().await.unwrap();
    assert_eq!(active.len(), 2);
    let ids: Vec<_> = active.iter().map(|r| r.spec.id.as_str()).collect();
    assert!(ids.contains(&"bd-1004"));
    assert!(ids.contains(&"bd-1007"));
}

#[tokio::test]
async fn sqlite_store_upsert_and_get_run() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    let store = SqliteStateStore::open(&db_path).unwrap();

    let run = make_run("bd-1100", 1, RunStatus::Launching);
    let run_id = run.run_id.clone();
    store.upsert_run(&run).await.unwrap();

    let fetched = store.get_run(&run_id).await.unwrap();
    assert!(fetched.is_some());
    let fetched = fetched.unwrap();
    assert_eq!(fetched.task_id.as_str(), "bd-1100");
    assert_eq!(fetched.attempt, 1);
    assert_eq!(fetched.status, RunStatus::Launching);
}

#[tokio::test]
async fn sqlite_store_active_runs_filters_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    let store = SqliteStateStore::open(&db_path).unwrap();

    let run1 = make_run("bd-1101", 1, RunStatus::Launching); // active
    let run2 = make_run("bd-1102", 1, RunStatus::Active); // active
    let run3 = make_run("bd-1103", 1, RunStatus::Completed); // terminal
    let run4 = make_run("bd-1104", 1, RunStatus::Failed); // terminal

    store.upsert_run(&run1).await.unwrap();
    store.upsert_run(&run2).await.unwrap();
    store.upsert_run(&run3).await.unwrap();
    store.upsert_run(&run4).await.unwrap();

    let active = store.active_runs().await.unwrap();
    assert_eq!(active.len(), 2);
}

#[tokio::test]
async fn sqlite_store_runs_for_task_filters_by_task() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    let store = SqliteStateStore::open(&db_path).unwrap();

    let run1 = make_run("bd-1200", 1, RunStatus::Completed);
    let run2 = make_run("bd-1200", 2, RunStatus::Active);
    let run3 = make_run("bd-1201", 1, RunStatus::Active);

    store.upsert_run(&run1).await.unwrap();
    store.upsert_run(&run2).await.unwrap();
    store.upsert_run(&run3).await.unwrap();

    let runs = store.runs_for_task(&TaskId::new("bd-1200")).await.unwrap();
    assert_eq!(runs.len(), 2);
    let attempts: Vec<_> = runs.iter().map(|r| r.attempt).collect();
    assert!(attempts.contains(&1));
    assert!(attempts.contains(&2));
}

#[tokio::test]
async fn sqlite_store_save_and_get_ticket() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    let store = SqliteStateStore::open(&db_path).unwrap();

    let request = InteractionRequest::new(
        TaskId::new("bd-1300"),
        RunId::new_v4(),
        InteractionRequestKind::ApprovalRequired {
            pr_url: "http://test".into(),
            pr_number: 42,
        },
        "Please review",
        "A PR is ready for your review",
        vec![InteractionAction::Approve, InteractionAction::Reject],
    );
    let ticket = InteractionTicket::new(request.clone());
    store.save_ticket(&ticket).await.unwrap();

    let fetched = store.get_ticket(&request.id).await.unwrap();
    assert!(fetched.is_some());
    let fetched = fetched.unwrap();
    assert_eq!(fetched.request.id, request.id);
    assert_eq!(fetched.request.task_id.as_str(), "bd-1300");
}

#[tokio::test]
async fn sqlite_store_pending_tickets_filters_resolved() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    let store = SqliteStateStore::open(&db_path).unwrap();

    let request1 = InteractionRequest::new(
        TaskId::new("bd-1301"),
        RunId::new_v4(),
        InteractionRequestKind::StuckNotification {
            reason: "timeout".into(),
        },
        "Stuck task",
        "A task needs attention",
        vec![InteractionAction::Retry, InteractionAction::Close],
    );
    let request2 = InteractionRequest::new(
        TaskId::new("bd-1302"),
        RunId::new_v4(),
        InteractionRequestKind::ContextNeeded {
            missing_fields: vec!["priority".into()],
        },
        "Need context",
        "Missing information",
        vec![InteractionAction::Ignore],
    );

    let ticket1 = InteractionTicket::new(request1.clone());
    let ticket2 = InteractionTicket::new(request2.clone());
    store.save_ticket(&ticket1).await.unwrap();
    store.save_ticket(&ticket2).await.unwrap();

    // Initially both pending
    let pending = store.pending_tickets().await.unwrap();
    assert_eq!(pending.len(), 2);

    // Resolve one
    let resolution = InteractionResolution {
        request_id: request1.id.clone(),
        task_id: TaskId::new("bd-1301"),
        run_id: request1.run_id.clone(),
        action: InteractionAction::Retry,
        note: None,
        resolved_at: chrono::Utc::now(),
        resolved_by: "test".into(),
    };
    store.resolve_ticket(&resolution).await.unwrap();

    // Now only one pending
    let pending = store.pending_tickets().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].request.id, request2.id);
}
