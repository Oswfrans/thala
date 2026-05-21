//! Callback receiver for remote execution backends.
//!
//! Modal and OpenCode Zen report completion by POSTing back to Thala. This
//! module owns the small HTTP surface for those callbacks and converts valid
//! callbacks into normal run/task transitions plus orchestrator events.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::extract::State;
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

use crate::core::events::OrchestratorEvent;
use crate::core::goal::ResearchReport;
use crate::core::ids::{RunId, TaskId};
use crate::core::run::{RunStatus, TaskRun};
use crate::core::task::TaskStatus;
use crate::core::transitions::{apply_run_transition, apply_transition, RunTransition, Transition};
use crate::ports::state_store::StateStore;

const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8788";

#[derive(Debug, Clone)]
pub struct CallbackServerConfig {
    pub bind_addr: SocketAddr,
}

impl CallbackServerConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let raw = std::env::var("THALA_CALLBACK_BIND").unwrap_or_else(|_| DEFAULT_BIND_ADDR.into());
        let bind_addr = raw
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid THALA_CALLBACK_BIND '{raw}': {e}"))?;
        Ok(Self { bind_addr })
    }
}

#[derive(Clone)]
pub struct CallbackServer {
    config: CallbackServerConfig,
    store: Arc<dyn StateStore>,
    events_tx: mpsc::Sender<OrchestratorEvent>,
}

impl CallbackServer {
    pub fn new(
        config: CallbackServerConfig,
        store: Arc<dyn StateStore>,
        events_tx: mpsc::Sender<OrchestratorEvent>,
    ) -> Self {
        Self {
            config,
            store,
            events_tx,
        }
    }

    pub async fn run(self) -> anyhow::Result<()> {
        let bind_addr = self.config.bind_addr;
        let listener = tokio::net::TcpListener::bind(bind_addr).await?;
        tracing::info!(%bind_addr, "Worker callback receiver listening");

        axum::serve(listener, self.router()).await?;
        Ok(())
    }

    fn router(self) -> Router {
        Router::new()
            .route("/api/worker/callback", post(worker_callback))
            .layer(DefaultBodyLimit::max(1024 * 1024)) // 1 MiB — callbacks carry no large payloads
            .with_state(CallbackState {
                store: self.store,
                events_tx: self.events_tx,
            })
    }
}

#[derive(Clone)]
struct CallbackState {
    store: Arc<dyn StateStore>,
    events_tx: mpsc::Sender<OrchestratorEvent>,
}

#[derive(Debug, Deserialize)]
struct WorkerCallbackPayload {
    task_id: String,
    #[serde(default)]
    run_id: Option<String>,
    status: String,
    #[serde(default)]
    exit_code: Option<i32>,
    #[serde(default)]
    error_message: Option<String>,
    /// Optional structured artifact path for non-PR workflows such as goal
    /// research fanout.
    #[serde(default)]
    report_artifact: Option<String>,
    /// Inline goal-mode research report. Remote goal workers must include this
    /// on successful completion because Thala does not fetch report files from
    /// remote git branches during synthesis.
    #[serde(default)]
    report: Option<ResearchReport>,
}

#[derive(Debug, Serialize)]
struct WorkerCallbackResponse {
    ok: bool,
    run_id: String,
    status: String,
}

async fn worker_callback(
    State(state): State<CallbackState>,
    headers: HeaderMap,
    Json(payload): Json<WorkerCallbackPayload>,
) -> impl IntoResponse {
    match handle_worker_callback(&state, &headers, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(err) => err.into_response(),
    }
}

#[derive(Debug)]
struct CallbackError {
    status: StatusCode,
    message: String,
}

impl CallbackError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, message)
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }
}

impl IntoResponse for CallbackError {
    fn into_response(self) -> axum::response::Response {
        let body = Json(serde_json::json!({
            "ok": false,
            "error": self.message,
        }));
        (self.status, body).into_response()
    }
}

async fn handle_worker_callback(
    state: &CallbackState,
    headers: &HeaderMap,
    payload: WorkerCallbackPayload,
) -> Result<WorkerCallbackResponse, CallbackError> {
    let token_hash = bearer_token_hash(headers)?;
    let task_id = TaskId::new(payload.task_id.clone());

    let record = state
        .store
        .get_task(&task_id)
        .await
        .map_err(storage_error)?;

    let run_id = payload
        .run_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(RunId::from)
        .or_else(|| {
            record
                .as_ref()
                .and_then(|record| record.active_run_id.clone())
        })
        .ok_or_else(|| CallbackError::bad_request("run_id missing and task has no active run"))?;

    let mut run = state
        .store
        .get_run(&run_id)
        .await
        .map_err(storage_error)?
        .ok_or_else(|| CallbackError::not_found("run not found"))?;

    if run.task_id != task_id {
        return Err(CallbackError::bad_request("run does not belong to task"));
    }

    let expected_hash = run
        .callback_token_hash
        .as_deref()
        .ok_or_else(|| CallbackError::unauthorized("run does not accept callbacks"))?;

    if !constant_time_eq(token_hash.as_bytes(), expected_hash.as_bytes()) {
        return Err(CallbackError::unauthorized("invalid callback token"));
    }

    if let Some(mut research_run) = state
        .store
        .research_run_for_worker(&run_id)
        .await
        .map_err(storage_error)?
    {
        if let Some(report_artifact) = payload.report_artifact.as_deref() {
            validate_report_artifact_path(report_artifact).map_err(CallbackError::bad_request)?;
            research_run.report_artifact = Some(report_artifact.to_string());
        }
        if let Some(report) = payload.report.as_ref() {
            validate_research_report_ids(&report, &research_run.goal_id, &run_id)
                .map_err(CallbackError::bad_request)?;
            research_run.report = Some(report.clone());
        }
        state
            .store
            .upsert_research_run(&research_run)
            .await
            .map_err(storage_error)?;
    }

    if run.status.is_terminal() {
        return Ok(WorkerCallbackResponse {
            ok: true,
            run_id: run.run_id.to_string(),
            status: run.status.as_str().into(),
        });
    }

    if run.status == RunStatus::Launching {
        run = apply_run_transition(&run, RunTransition::Activated).map_err(state_error)?;
        state.store.upsert_run(&run).await.map_err(storage_error)?;
    }

    match normalized_status(&payload.status) {
        CallbackOutcome::Success => complete_run(state, &run).await,
        CallbackOutcome::Failure => fail_run(state, &run, &payload).await,
    }
}

async fn complete_run(
    state: &CallbackState,
    run: &TaskRun,
) -> Result<WorkerCallbackResponse, CallbackError> {
    let updated =
        apply_run_transition(run, RunTransition::CompletionSignaled).map_err(state_error)?;
    state
        .store
        .upsert_run(&updated)
        .await
        .map_err(storage_error)?;

    let _ = state
        .events_tx
        .send(OrchestratorEvent::run_completed(
            updated.task_id.clone(),
            updated.run_id.clone(),
        ))
        .await;

    Ok(WorkerCallbackResponse {
        ok: true,
        run_id: updated.run_id.to_string(),
        status: updated.status.as_str().into(),
    })
}

async fn fail_run(
    state: &CallbackState,
    run: &TaskRun,
    payload: &WorkerCallbackPayload,
) -> Result<WorkerCallbackResponse, CallbackError> {
    let reason = payload.error_message.as_deref().map_or_else(
        || {
            payload.exit_code.map_or_else(
                || "worker callback reported failure".to_string(),
                |code| format!("worker callback reported failure with exit code {code}"),
            )
        },
        ToString::to_string,
    );

    let updated = apply_run_transition(
        run,
        RunTransition::FailureSignaled {
            reason: reason.clone(),
        },
    )
    .map_err(state_error)?;
    state
        .store
        .upsert_run(&updated)
        .await
        .map_err(storage_error)?;

    if let Some(record) = state
        .store
        .get_task(&updated.task_id)
        .await
        .map_err(storage_error)?
        .filter(|r| r.status == TaskStatus::Running)
    {
        let failed = apply_transition(
            &record,
            Transition::RunFailed {
                reason: reason.clone(),
            },
        )
        .map_err(state_error)?;
        state
            .store
            .upsert_task(&failed)
            .await
            .map_err(storage_error)?;
    }

    let _ = state
        .events_tx
        .send(OrchestratorEvent::run_failed(
            updated.task_id.clone(),
            updated.run_id.clone(),
            reason,
        ))
        .await;

    Ok(WorkerCallbackResponse {
        ok: true,
        run_id: updated.run_id.to_string(),
        status: updated.status.as_str().into(),
    })
}

enum CallbackOutcome {
    Success,
    Failure,
}

fn normalized_status(status: &str) -> CallbackOutcome {
    match status.trim().to_ascii_lowercase().as_str() {
        "success" | "completed" | "complete" | "done" | "succeeded" => CallbackOutcome::Success,
        _ => CallbackOutcome::Failure,
    }
}

fn bearer_token_hash(headers: &HeaderMap) -> Result<String, CallbackError> {
    let value = headers
        .get(AUTHORIZATION)
        .ok_or_else(|| CallbackError::unauthorized("missing Authorization header"))?
        .to_str()
        .map_err(|_| CallbackError::unauthorized("invalid Authorization header"))?;
    let token = value
        .strip_prefix("Bearer ")
        .ok_or_else(|| CallbackError::unauthorized("expected bearer token"))?;
    Ok(sha256_hex(token.as_bytes()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

fn validate_report_artifact_path(path: &str) -> Result<(), String> {
    let path = Path::new(path);
    if path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(
            "research report artifact path must be relative and stay inside workspace".into(),
        );
    }
    Ok(())
}

fn validate_research_report_ids(
    report: &ResearchReport,
    goal_id: &crate::core::ids::GoalId,
    run_id: &RunId,
) -> Result<(), String> {
    if report.goal_id != goal_id.as_str() || report.run_id != run_id.as_str() {
        return Err("research report IDs do not match expected goal/run".into());
    }
    Ok(())
}

fn storage_error(err: crate::core::error::ThalaError) -> CallbackError {
    CallbackError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

fn state_error(err: crate::core::state::StateError) -> CallbackError {
    CallbackError::bad_request(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use tokio::sync::mpsc;

    use crate::adapters::state::SqliteStateStore;
    use crate::core::goal::{
        Confidence, ResearchFinding, ResearchReport, ResearchRun, ResearchRunStatus,
    };
    use crate::core::ids::GoalId;
    use crate::core::ids::RunId;
    use crate::core::run::{ExecutionBackendKind, TaskRun};
    use crate::core::task::{TaskRecord, TaskSpec, TaskStatus};
    use crate::ports::state_store::StateStore;

    fn task_spec(id: &str) -> TaskSpec {
        TaskSpec {
            id: TaskId::new(id),
            title: "Remote callback task".into(),
            acceptance_criteria: "Callback marks completion".into(),
            context: String::new(),
            beads_ref: id.into(),
            model_override: None,
            always_human_review: false,
            labels: Vec::new(),
        }
    }

    fn research_report(goal_id: &GoalId, run_id: &RunId) -> ResearchReport {
        ResearchReport {
            goal_id: goal_id.as_str().to_string(),
            run_id: run_id.as_str().to_string(),
            variant: "risk-scan".into(),
            summary: "summary".into(),
            findings: vec![ResearchFinding {
                claim: "claim".into(),
                evidence: "evidence".into(),
                source: None,
                confidence: Confidence::High,
            }],
            recommendation: "recommendation".into(),
            risks: Vec::new(),
            follow_up_questions: Vec::new(),
            suggested_tasks: Vec::new(),
        }
    }

    #[tokio::test]
    async fn valid_callback_completes_active_run_and_emits_event() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteStateStore::open(tmp.path().join("state.db")).unwrap());
        let (events_tx, mut events_rx) = mpsc::channel(4);
        let state = CallbackState {
            store: store.clone(),
            events_tx,
        };

        let task_id = TaskId::new("bd-callback");
        let run_id = RunId::from("run-callback");
        let mut record = TaskRecord::new(task_spec(task_id.as_str()));
        record.status = TaskStatus::Running;
        record.active_run_id = Some(run_id.clone());
        store.upsert_task(&record).await.unwrap();

        let mut run = TaskRun::new(
            run_id.clone(),
            task_id.clone(),
            1,
            ExecutionBackendKind::Modal,
        );
        run.status = RunStatus::Active;
        run.callback_token_hash = Some(sha256_hex(b"secret-token"));
        store.upsert_run(&run).await.unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer secret-token"),
        );

        let response = handle_worker_callback(
            &state,
            &headers,
            WorkerCallbackPayload {
                task_id: task_id.to_string(),
                run_id: Some(run_id.to_string()),
                status: "success".into(),
                exit_code: Some(0),
                error_message: None,
                report_artifact: None,
                report: None,
            },
        )
        .await
        .unwrap();

        assert!(response.ok);
        let updated = store.get_run(&run_id).await.unwrap().unwrap();
        assert_eq!(updated.status, RunStatus::Completed);

        match events_rx.recv().await.unwrap() {
            OrchestratorEvent::RunCompleted {
                task_id: event_task_id,
                run_id: event_run_id,
                ..
            } => {
                assert_eq!(event_task_id, task_id);
                assert_eq!(event_run_id, run_id);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn invalid_callback_token_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteStateStore::open(tmp.path().join("state.db")).unwrap());
        let (events_tx, _events_rx) = mpsc::channel(4);
        let state = CallbackState {
            store: store.clone(),
            events_tx,
        };

        let task_id = TaskId::new("bd-callback");
        let run_id = RunId::from("run-callback");
        let mut record = TaskRecord::new(task_spec(task_id.as_str()));
        record.status = TaskStatus::Running;
        record.active_run_id = Some(run_id.clone());
        store.upsert_task(&record).await.unwrap();

        let mut run = TaskRun::new(
            run_id.clone(),
            task_id.clone(),
            1,
            ExecutionBackendKind::Modal,
        );
        run.status = RunStatus::Active;
        run.callback_token_hash = Some(sha256_hex(b"secret-token"));
        store.upsert_run(&run).await.unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer wrong-token"),
        );

        let err = handle_worker_callback(
            &state,
            &headers,
            WorkerCallbackPayload {
                task_id: task_id.to_string(),
                run_id: Some(run_id.to_string()),
                status: "success".into(),
                exit_code: Some(0),
                error_message: None,
                report_artifact: None,
                report: None,
            },
        )
        .await
        .unwrap_err();

        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn invalid_callback_token_cannot_update_research_artifact() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteStateStore::open(tmp.path().join("state.db")).unwrap());
        let (events_tx, _events_rx) = mpsc::channel(4);
        let state = CallbackState {
            store: store.clone(),
            events_tx,
        };

        let task_id = TaskId::new("bd-callback-research");
        let run_id = RunId::from("run-callback-research");
        let goal_id = GoalId::new("goal-callback-research");
        let mut record = TaskRecord::new(task_spec(task_id.as_str()));
        record.status = TaskStatus::Running;
        record.active_run_id = Some(run_id.clone());
        store.upsert_task(&record).await.unwrap();

        let mut run = TaskRun::new(
            run_id.clone(),
            task_id.clone(),
            1,
            ExecutionBackendKind::Modal,
        );
        run.status = RunStatus::Active;
        run.callback_token_hash = Some(sha256_hex(b"secret-token"));
        store.upsert_run(&run).await.unwrap();
        store
            .upsert_research_run(&ResearchRun {
                research_run_id: RunId::new_v4(),
                goal_id: goal_id.clone(),
                source_task_id: task_id.clone(),
                child_task_id: task_id.clone(),
                variant_name: "risk-scan".into(),
                prompt: "research".into(),
                backend: ExecutionBackendKind::Modal,
                worker_run_id: run_id.clone(),
                status: ResearchRunStatus::Running,
                research_round: 1,
                report_artifact: Some(".thala/research/original.json".into()),
                report: None,
                failure: None,
            })
            .await
            .unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer wrong-token"),
        );

        let err = handle_worker_callback(
            &state,
            &headers,
            WorkerCallbackPayload {
                task_id: task_id.to_string(),
                run_id: Some(run_id.to_string()),
                status: "success".into(),
                exit_code: Some(0),
                error_message: None,
                report_artifact: Some(".thala/research/poisoned.json".into()),
                report: None,
            },
        )
        .await
        .unwrap_err();

        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
        let research = store
            .research_run_for_worker(&run_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            research.report_artifact.as_deref(),
            Some(".thala/research/original.json")
        );
    }

    #[tokio::test]
    async fn valid_callback_stores_inline_research_report() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteStateStore::open(tmp.path().join("state.db")).unwrap());
        let (events_tx, _events_rx) = mpsc::channel(4);
        let state = CallbackState {
            store: store.clone(),
            events_tx,
        };

        let task_id = TaskId::new("bd-callback-inline-report");
        let run_id = RunId::from("run-callback-inline-report");
        let goal_id = GoalId::new("goal-callback-inline-report");
        let mut record = TaskRecord::new(task_spec(task_id.as_str()));
        record.status = TaskStatus::Running;
        record.active_run_id = Some(run_id.clone());
        store.upsert_task(&record).await.unwrap();

        let mut run = TaskRun::new(
            run_id.clone(),
            task_id.clone(),
            1,
            ExecutionBackendKind::Modal,
        );
        run.status = RunStatus::Active;
        run.callback_token_hash = Some(sha256_hex(b"secret-token"));
        store.upsert_run(&run).await.unwrap();
        store
            .upsert_research_run(&ResearchRun {
                research_run_id: RunId::new_v4(),
                goal_id: goal_id.clone(),
                source_task_id: task_id.clone(),
                child_task_id: task_id.clone(),
                variant_name: "risk-scan".into(),
                prompt: "research".into(),
                backend: ExecutionBackendKind::Modal,
                worker_run_id: run_id.clone(),
                status: ResearchRunStatus::Running,
                research_round: 1,
                report_artifact: Some(".thala/research/original.json".into()),
                report: None,
                failure: None,
            })
            .await
            .unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer secret-token"),
        );
        let report = research_report(&goal_id, &run_id);

        let response = handle_worker_callback(
            &state,
            &headers,
            WorkerCallbackPayload {
                task_id: task_id.to_string(),
                run_id: Some(run_id.to_string()),
                status: "success".into(),
                exit_code: Some(0),
                error_message: None,
                report_artifact: Some(".thala/research/inline.json".into()),
                report: Some(report.clone()),
            },
        )
        .await
        .unwrap();

        assert!(response.ok);
        let stored = store
            .research_run_for_worker(&run_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.report, Some(report));
        assert_eq!(
            stored.report_artifact.as_deref(),
            Some(".thala/research/inline.json")
        );
    }

    #[tokio::test]
    async fn empty_callback_run_id_falls_back_to_active_run() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteStateStore::open(tmp.path().join("state.db")).unwrap());
        let (events_tx, mut events_rx) = mpsc::channel(4);
        let state = CallbackState {
            store: store.clone(),
            events_tx,
        };

        let task_id = TaskId::new("bd-callback-empty-run");
        let run_id = RunId::from("run-callback-active");
        let mut record = TaskRecord::new(task_spec(task_id.as_str()));
        record.status = TaskStatus::Running;
        record.active_run_id = Some(run_id.clone());
        store.upsert_task(&record).await.unwrap();

        let mut run = TaskRun::new(
            run_id.clone(),
            task_id.clone(),
            1,
            ExecutionBackendKind::Modal,
        );
        run.status = RunStatus::Active;
        run.callback_token_hash = Some(sha256_hex(b"secret-token"));
        store.upsert_run(&run).await.unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer secret-token"),
        );

        let response = handle_worker_callback(
            &state,
            &headers,
            WorkerCallbackPayload {
                task_id: task_id.to_string(),
                run_id: Some(String::new()),
                status: "success".into(),
                exit_code: Some(0),
                error_message: None,
                report_artifact: None,
                report: None,
            },
        )
        .await
        .unwrap();

        assert!(response.ok);
        assert_eq!(response.run_id, run_id.as_str());
        let updated = store.get_run(&run_id).await.unwrap().unwrap();
        assert_eq!(updated.status, RunStatus::Completed);

        match events_rx.recv().await.unwrap() {
            OrchestratorEvent::RunCompleted {
                task_id: event_task_id,
                run_id: event_run_id,
                ..
            } => {
                assert_eq!(event_task_id, task_id);
                assert_eq!(event_run_id, run_id);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
