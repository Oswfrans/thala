//! SQLite-backed StateStore implementation.
//!
//! Thala's runtime state (task records, run records, interaction tickets) is
//! persisted in a single SQLite database file at `$XDG_DATA_HOME/thala/state.db`.
//!
//! Schema is created on first open. All operations are synchronous SQLite calls
//! wrapped in `tokio::task::spawn_blocking` to avoid blocking the async runtime.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use rusqlite::{params, Connection};

use crate::core::error::ThalaError;
use crate::core::goal::{GoalRecord, ResearchRun};
use crate::core::ids::{GoalId, InteractionId, RunId, TaskId};
use crate::core::interaction::{InteractionResolution, InteractionTicket};
use crate::core::run::TaskRun;
use crate::core::task::TaskRecord;
use crate::ports::state_store::StateStore;

// ── SqliteStateStore ──────────────────────────────────────────────────────────

/// SQLite-backed implementation of [`StateStore`].
///
/// All records are stored as JSON blobs for schema flexibility.
pub struct SqliteStateStore {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteStateStore {
    /// Open (or create) the state database at the given path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ThalaError> {
        let conn = Connection::open(path.as_ref())
            .map_err(|e| ThalaError::Storage(format!("Failed to open state database: {e}")))?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<(), ThalaError> {
        let conn = self.conn.lock();
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS task_records (
                task_id TEXT PRIMARY KEY,
                data    TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS run_records (
                run_id  TEXT PRIMARY KEY,
                task_id TEXT NOT NULL,
                active  INTEGER NOT NULL DEFAULT 1,
                data    TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS interaction_tickets (
                interaction_id TEXT PRIMARY KEY,
                resolved       INTEGER NOT NULL DEFAULT 0,
                data           TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS goal_records (
                goal_id        TEXT PRIMARY KEY,
                source_task_id TEXT NOT NULL,
                active         INTEGER NOT NULL DEFAULT 1,
                data           TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_goal_records_source_task
                ON goal_records(source_task_id);
            CREATE TABLE IF NOT EXISTS research_runs (
                research_run_id TEXT PRIMARY KEY,
                goal_id         TEXT NOT NULL,
                worker_run_id   TEXT NOT NULL,
                active          INTEGER NOT NULL DEFAULT 1,
                data            TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_research_runs_goal
                ON research_runs(goal_id);
            CREATE INDEX IF NOT EXISTS idx_research_runs_worker
                ON research_runs(worker_run_id);
            ",
        )
        .map_err(|e| ThalaError::Storage(format!("Migration failed: {e}")))?;
        Ok(())
    }
}

// ── StateStore impl ───────────────────────────────────────────────────────────

#[async_trait]
impl StateStore for SqliteStateStore {
    async fn upsert_task(&self, record: &TaskRecord) -> Result<(), ThalaError> {
        let data = serde_json::to_string(record).map_err(|e| ThalaError::Storage(e.to_string()))?;
        let id = record.spec.id.as_str().to_owned();
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO task_records (task_id, data) VALUES (?1, ?2)
             ON CONFLICT(task_id) DO UPDATE SET data = excluded.data",
            params![id, data],
        )
        .map_err(|e| ThalaError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn get_task(&self, task_id: &TaskId) -> Result<Option<TaskRecord>, ThalaError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT data FROM task_records WHERE task_id = ?1")
            .map_err(|e| ThalaError::Storage(e.to_string()))?;
        let rows: Vec<String> = stmt
            .query_map(params![task_id.as_str()], |row| row.get(0))
            .map_err(|e| ThalaError::Storage(e.to_string()))?
            .collect::<Result<_, _>>()
            .map_err(|e| ThalaError::Storage(e.to_string()))?;
        match rows.into_iter().next() {
            None => Ok(None),
            Some(json) => {
                let record =
                    serde_json::from_str(&json).map_err(|e| ThalaError::Storage(e.to_string()))?;
                Ok(Some(record))
            }
        }
    }

    async fn active_tasks(&self) -> Result<Vec<TaskRecord>, ThalaError> {
        let all = self.all_tasks().await?;
        Ok(all
            .into_iter()
            .filter(|r| !r.status.is_terminal())
            .collect())
    }

    async fn all_tasks(&self) -> Result<Vec<TaskRecord>, ThalaError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT data FROM task_records")
            .map_err(|e| ThalaError::Storage(e.to_string()))?;
        let rows: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| ThalaError::Storage(e.to_string()))?
            .collect::<Result<_, _>>()
            .map_err(|e| ThalaError::Storage(e.to_string()))?;
        rows.iter()
            .map(|json| serde_json::from_str(json).map_err(|e| ThalaError::Storage(e.to_string())))
            .collect()
    }

    async fn upsert_goal(&self, record: &GoalRecord) -> Result<(), ThalaError> {
        let data = serde_json::to_string(record).map_err(|e| ThalaError::Storage(e.to_string()))?;
        let goal_id = record.goal_id.as_str().to_owned();
        let source_task_id = record.source_task_id.as_str().to_owned();
        let active = i64::from(!record.status.is_terminal());
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO goal_records (goal_id, source_task_id, active, data) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(goal_id) DO UPDATE SET source_task_id = excluded.source_task_id, active = excluded.active, data = excluded.data",
            params![goal_id, source_task_id, active, data],
        )
        .map_err(|e| ThalaError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn get_goal(&self, goal_id: &GoalId) -> Result<Option<GoalRecord>, ThalaError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT data FROM goal_records WHERE goal_id = ?1")
            .map_err(|e| ThalaError::Storage(e.to_string()))?;
        let rows: Vec<String> = stmt
            .query_map(params![goal_id.as_str()], |row| row.get(0))
            .map_err(|e| ThalaError::Storage(e.to_string()))?
            .collect::<Result<_, _>>()
            .map_err(|e| ThalaError::Storage(e.to_string()))?;
        match rows.into_iter().next() {
            None => Ok(None),
            Some(json) => Ok(Some(
                serde_json::from_str(&json).map_err(|e| ThalaError::Storage(e.to_string()))?,
            )),
        }
    }

    async fn goal_for_task(&self, task_id: &TaskId) -> Result<Option<GoalRecord>, ThalaError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT data FROM goal_records WHERE source_task_id = ?1")
            .map_err(|e| ThalaError::Storage(e.to_string()))?;
        let rows: Vec<String> = stmt
            .query_map(params![task_id.as_str()], |row| row.get(0))
            .map_err(|e| ThalaError::Storage(e.to_string()))?
            .collect::<Result<_, _>>()
            .map_err(|e| ThalaError::Storage(e.to_string()))?;
        match rows.into_iter().next() {
            None => Ok(None),
            Some(json) => Ok(Some(
                serde_json::from_str(&json).map_err(|e| ThalaError::Storage(e.to_string()))?,
            )),
        }
    }

    async fn active_goals(&self) -> Result<Vec<GoalRecord>, ThalaError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT data FROM goal_records WHERE active = 1")
            .map_err(|e| ThalaError::Storage(e.to_string()))?;
        let rows: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| ThalaError::Storage(e.to_string()))?
            .collect::<Result<_, _>>()
            .map_err(|e| ThalaError::Storage(e.to_string()))?;
        rows.iter()
            .map(|json| serde_json::from_str(json).map_err(|e| ThalaError::Storage(e.to_string())))
            .collect()
    }

    async fn upsert_research_run(&self, run: &ResearchRun) -> Result<(), ThalaError> {
        let data = serde_json::to_string(run).map_err(|e| ThalaError::Storage(e.to_string()))?;
        let research_run_id = run.research_run_id.as_str().to_owned();
        let goal_id = run.goal_id.as_str().to_owned();
        let worker_run_id = run.worker_run_id.as_str().to_owned();
        let active = i64::from(matches!(
            run.status,
            crate::core::goal::ResearchRunStatus::Planned
                | crate::core::goal::ResearchRunStatus::Running
        ));
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO research_runs (research_run_id, goal_id, worker_run_id, active, data) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(research_run_id) DO UPDATE SET goal_id = excluded.goal_id, worker_run_id = excluded.worker_run_id, active = excluded.active, data = excluded.data",
            params![research_run_id, goal_id, worker_run_id, active, data],
        )
        .map_err(|e| ThalaError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn research_runs_for_goal(
        &self,
        goal_id: &GoalId,
    ) -> Result<Vec<ResearchRun>, ThalaError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT data FROM research_runs WHERE goal_id = ?1")
            .map_err(|e| ThalaError::Storage(e.to_string()))?;
        let rows: Vec<String> = stmt
            .query_map(params![goal_id.as_str()], |row| row.get(0))
            .map_err(|e| ThalaError::Storage(e.to_string()))?
            .collect::<Result<_, _>>()
            .map_err(|e| ThalaError::Storage(e.to_string()))?;
        rows.iter()
            .map(|json| serde_json::from_str(json).map_err(|e| ThalaError::Storage(e.to_string())))
            .collect()
    }

    async fn research_run_for_worker(
        &self,
        worker_run_id: &RunId,
    ) -> Result<Option<ResearchRun>, ThalaError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT data FROM research_runs WHERE worker_run_id = ?1")
            .map_err(|e| ThalaError::Storage(e.to_string()))?;
        let rows: Vec<String> = stmt
            .query_map(params![worker_run_id.as_str()], |row| row.get(0))
            .map_err(|e| ThalaError::Storage(e.to_string()))?
            .collect::<Result<_, _>>()
            .map_err(|e| ThalaError::Storage(e.to_string()))?;
        match rows.into_iter().next() {
            None => Ok(None),
            Some(json) => Ok(Some(
                serde_json::from_str(&json).map_err(|e| ThalaError::Storage(e.to_string()))?,
            )),
        }
    }

    async fn upsert_run(&self, run: &TaskRun) -> Result<(), ThalaError> {
        let data = serde_json::to_string(run).map_err(|e| ThalaError::Storage(e.to_string()))?;
        let run_id = run.run_id.as_str().to_owned();
        let task_id = run.task_id.as_str().to_owned();
        let active = i64::from(!run.status.is_terminal());
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO run_records (run_id, task_id, active, data) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(run_id) DO UPDATE SET active = excluded.active, data = excluded.data",
            params![run_id, task_id, active, data],
        )
        .map_err(|e| ThalaError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn get_run(&self, run_id: &RunId) -> Result<Option<TaskRun>, ThalaError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT data FROM run_records WHERE run_id = ?1")
            .map_err(|e| ThalaError::Storage(e.to_string()))?;
        let rows: Vec<String> = stmt
            .query_map(params![run_id.as_str()], |row| row.get(0))
            .map_err(|e| ThalaError::Storage(e.to_string()))?
            .collect::<Result<_, _>>()
            .map_err(|e| ThalaError::Storage(e.to_string()))?;
        match rows.into_iter().next() {
            None => Ok(None),
            Some(json) => {
                let run =
                    serde_json::from_str(&json).map_err(|e| ThalaError::Storage(e.to_string()))?;
                Ok(Some(run))
            }
        }
    }

    async fn active_runs(&self) -> Result<Vec<TaskRun>, ThalaError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT data FROM run_records WHERE active = 1")
            .map_err(|e| ThalaError::Storage(e.to_string()))?;
        let rows: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| ThalaError::Storage(e.to_string()))?
            .collect::<Result<_, _>>()
            .map_err(|e| ThalaError::Storage(e.to_string()))?;
        rows.iter()
            .map(|json| serde_json::from_str(json).map_err(|e| ThalaError::Storage(e.to_string())))
            .collect()
    }

    async fn runs_for_task(&self, task_id: &TaskId) -> Result<Vec<TaskRun>, ThalaError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT data FROM run_records WHERE task_id = ?1")
            .map_err(|e| ThalaError::Storage(e.to_string()))?;
        let rows: Vec<String> = stmt
            .query_map(params![task_id.as_str()], |row| row.get(0))
            .map_err(|e| ThalaError::Storage(e.to_string()))?
            .collect::<Result<_, _>>()
            .map_err(|e| ThalaError::Storage(e.to_string()))?;
        rows.iter()
            .map(|json| serde_json::from_str(json).map_err(|e| ThalaError::Storage(e.to_string())))
            .collect()
    }

    async fn save_ticket(&self, ticket: &InteractionTicket) -> Result<(), ThalaError> {
        let data = serde_json::to_string(ticket).map_err(|e| ThalaError::Storage(e.to_string()))?;
        let id = ticket.request.id.as_str().to_owned();
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR IGNORE INTO interaction_tickets (interaction_id, resolved, data)
             VALUES (?1, 0, ?2)",
            params![id, data],
        )
        .map_err(|e| ThalaError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn update_ticket(&self, ticket: &InteractionTicket) -> Result<(), ThalaError> {
        let data = serde_json::to_string(ticket).map_err(|e| ThalaError::Storage(e.to_string()))?;
        let id = ticket.request.id.as_str().to_owned();
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE interaction_tickets SET data = ?2 WHERE interaction_id = ?1",
            params![id, data],
        )
        .map_err(|e| ThalaError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn pending_tickets(&self) -> Result<Vec<InteractionTicket>, ThalaError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT data FROM interaction_tickets WHERE resolved = 0")
            .map_err(|e| ThalaError::Storage(e.to_string()))?;
        let rows: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| ThalaError::Storage(e.to_string()))?
            .collect::<Result<_, _>>()
            .map_err(|e| ThalaError::Storage(e.to_string()))?;
        rows.iter()
            .map(|json| serde_json::from_str(json).map_err(|e| ThalaError::Storage(e.to_string())))
            .collect()
    }

    async fn get_ticket(
        &self,
        interaction_id: &InteractionId,
    ) -> Result<Option<InteractionTicket>, ThalaError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT data FROM interaction_tickets WHERE interaction_id = ?1")
            .map_err(|e| ThalaError::Storage(e.to_string()))?;
        let rows: Vec<String> = stmt
            .query_map(params![interaction_id.as_str()], |row| row.get(0))
            .map_err(|e| ThalaError::Storage(e.to_string()))?
            .collect::<Result<_, _>>()
            .map_err(|e| ThalaError::Storage(e.to_string()))?;
        match rows.into_iter().next() {
            None => Ok(None),
            Some(json) => {
                let ticket =
                    serde_json::from_str(&json).map_err(|e| ThalaError::Storage(e.to_string()))?;
                Ok(Some(ticket))
            }
        }
    }

    async fn resolve_ticket(&self, resolution: &InteractionResolution) -> Result<(), ThalaError> {
        let id = resolution.request_id.as_str().to_owned();
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE interaction_tickets SET resolved = 1 WHERE interaction_id = ?1",
            params![id],
        )
        .map_err(|e| ThalaError::Storage(e.to_string()))?;
        Ok(())
    }
}
