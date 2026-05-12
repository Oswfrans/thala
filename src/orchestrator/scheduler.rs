//! Scheduler — polls Beads and identifies tasks ready to dispatch.
//!
//! The scheduler does not dispatch tasks itself. It emits DispatchReady events
//! for the dispatcher to consume. This keeps scheduling policy separate from
//! dispatch mechanics.
//!
//! Flow each tick:
//!   1. Fetch ready tasks from TaskSource (Beads).
//!   2. Load active task records from StateStore.
//!   3. Deduplicate: skip tasks already active in this Thala instance.
//!   4. Check concurrency headroom.
//!   5. Emit DispatchReady for each dispatchable task.

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

use crate::core::events::OrchestratorEvent;
use crate::ports::state_store::StateStore;
use crate::ports::task_source::TaskSource;

// ── SchedulerConfig ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// How often to poll Beads for new tasks.
    pub poll_interval: Duration,

    /// Maximum number of concurrently active runs.
    pub max_concurrent_runs: usize,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(30),
            max_concurrent_runs: 3,
        }
    }
}

// ── Scheduler ─────────────────────────────────────────────────────────────────

pub struct Scheduler {
    config: SchedulerConfig,
    source: Arc<dyn TaskSource>,
    store: Arc<dyn StateStore>,
    events: mpsc::Sender<OrchestratorEvent>,
}

impl Scheduler {
    pub fn new(
        config: SchedulerConfig,
        source: Arc<dyn TaskSource>,
        store: Arc<dyn StateStore>,
        events: mpsc::Sender<OrchestratorEvent>,
    ) -> Self {
        Self {
            config,
            source,
            store,
            events,
        }
    }

    /// Run the scheduler loop. This task runs until the process exits.
    pub async fn run(self) {
        tracing::info!(
            poll_interval_secs = self.config.poll_interval.as_secs(),
            max_concurrent = self.config.max_concurrent_runs,
            "Scheduler starting"
        );

        loop {
            if let Err(e) = self.tick().await {
                tracing::error!("Scheduler tick failed: {e}");
            }
            sleep(self.config.poll_interval).await;
        }
    }

    /// One scheduler tick. Returns the number of DispatchReady events emitted.
    async fn tick(&self) -> anyhow::Result<usize> {
        // Load active tasks to check concurrency.
        let active = self.store.active_tasks().await?;

        let active_count = active
            .iter()
            .filter(|r| {
                matches!(
                    r.status,
                    crate::core::task::TaskStatus::Dispatching
                        | crate::core::task::TaskStatus::Running
                        | crate::core::task::TaskStatus::WaitingForHuman
                        | crate::core::task::TaskStatus::Validating
                )
            })
            .count();

        if active_count >= self.config.max_concurrent_runs {
            tracing::debug!(
                active = active_count,
                max = self.config.max_concurrent_runs,
                "At max concurrency — skipping poll"
            );
            return Ok(0);
        }

        let headroom = self.config.max_concurrent_runs - active_count;

        // Fetch ready tasks from Beads.
        let ready = self.source.fetch_ready().await?;

        if ready.is_empty() {
            tracing::debug!("No ready tasks found in Beads");
            return Ok(0);
        }

        // Build a set of task IDs that are already in-flight. Ready records are
        // eligible for dispatch; they can appear after recovery or restart.
        let in_flight_ids: std::collections::HashSet<String> = active
            .iter()
            .filter(|r| {
                matches!(
                    r.status,
                    crate::core::task::TaskStatus::Dispatching
                        | crate::core::task::TaskStatus::Running
                        | crate::core::task::TaskStatus::WaitingForHuman
                        | crate::core::task::TaskStatus::Validating
                )
            })
            .map(|r| r.spec.id.as_str().to_string())
            .collect();

        let mut dispatched = 0;

        for spec in ready.into_iter().take(headroom) {
            // Skip tasks already being handled.
            if in_flight_ids.contains(spec.id.as_str()) {
                tracing::debug!(task_id = %spec.id, "Task already in-flight, skipping");
                continue;
            }

            // Skip tasks with no acceptance criteria.
            if !spec.is_dispatchable() {
                tracing::debug!(task_id = %spec.id, "Task has no acceptance criteria, skipping");
                continue;
            }

            tracing::info!(task_id = %spec.id, title = %spec.title, "Emitting DispatchReady");

            if self
                .events
                .send(OrchestratorEvent::dispatch_ready(spec.id.clone()))
                .await
                .is_err()
            {
                tracing::warn!("Event channel closed; scheduler stopping");
                return Ok(dispatched);
            }

            dispatched += 1;
        }

        Ok(dispatched)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    use crate::adapters::state::SqliteStateStore;
    use crate::core::error::ThalaError;
    use crate::core::ids::TaskId;
    use crate::core::task::{TaskRecord, TaskSpec, TaskStatus};

    struct StaticTaskSource {
        ready: Vec<TaskSpec>,
    }

    #[async_trait]
    impl TaskSource for StaticTaskSource {
        async fn fetch_ready(&self) -> Result<Vec<TaskSpec>, ThalaError> {
            Ok(self.ready.clone())
        }

        async fn fetch_by_id(&self, _task_id: &str) -> Result<Option<TaskSpec>, ThalaError> {
            Ok(None)
        }
    }

    fn task_spec(id: &str) -> TaskSpec {
        TaskSpec {
            id: TaskId::new(id),
            title: "Recovered task".into(),
            acceptance_criteria: "Dispatch this task".into(),
            context: String::new(),
            beads_ref: id.into(),
            model_override: None,
            always_human_review: false,
            labels: Vec::new(),
        }
    }

    #[tokio::test]
    async fn tick_dispatches_ready_local_record_returned_by_source() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteStateStore::open(dir.path().join("state.db")).unwrap());
        let spec = task_spec("bd-recovered");
        let mut record = TaskRecord::new(spec.clone());
        record.status = TaskStatus::Ready;
        store.upsert_task(&record).await.unwrap();

        let source = Arc::new(StaticTaskSource { ready: vec![spec] });
        let (events_tx, mut events_rx) = mpsc::channel(1);
        let scheduler = Scheduler::new(SchedulerConfig::default(), source, store, events_tx);

        assert_eq!(scheduler.tick().await.unwrap(), 1);

        match events_rx.try_recv().unwrap() {
            OrchestratorEvent::DispatchReady { task_id, .. } => {
                assert_eq!(task_id.as_str(), "bd-recovered");
            }
            event => panic!("unexpected event: {event:?}"),
        }
    }
}
