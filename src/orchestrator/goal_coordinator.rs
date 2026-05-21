//! Goal coordinator — opt-in research fanout for goal-mode tasks.
//!
//! Normal coding tasks continue through the scheduler → dispatcher → monitor →
//! validator path. This subsystem only handles tasks explicitly labelled
//! `mode:goal` when `goals.enabled = true`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

use crate::core::events::OrchestratorEvent;
use serde_json::Value;

use crate::core::goal::{
    GoalDecisionKind, GoalRecord, GoalStatus, GoalSynthesisDecision, ResearchReport, ResearchRun,
    ResearchRunStatus,
};
use crate::core::ids::{GoalId, RunId, TaskId};
use crate::core::run::TaskRun;
use crate::core::task::{TaskRecord, TaskSpec, TaskStatus};
use crate::core::transitions::{apply_run_transition, apply_transition, RunTransition, Transition};
use crate::core::workflow::WorkflowConfig;
use crate::ports::backend_router::BackendRouter;
use crate::ports::execution::LaunchRequest;
use crate::ports::repo::RepoProvider;
use crate::ports::state_store::StateStore;
use crate::ports::task_sink::{NewTaskRequest, TaskSink};
use crate::ports::task_source::TaskSource;

#[derive(Debug, serde::Deserialize)]
struct ResearchPlan {
    variants: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct GoalCoordinatorConfig {
    pub workspace_root: PathBuf,
    pub product: String,
}

pub struct GoalCoordinator {
    config: GoalCoordinatorConfig,
    workflow: WorkflowConfig,
    source: Arc<dyn TaskSource>,
    sink: Arc<dyn TaskSink>,
    store: Arc<dyn StateStore>,
    router: Arc<dyn BackendRouter>,
    repo: Arc<dyn RepoProvider>,
    events_tx: mpsc::Sender<OrchestratorEvent>,
    synthesis_lock: Arc<tokio::sync::Mutex<()>>,
}

impl GoalCoordinator {
    pub fn new(
        config: GoalCoordinatorConfig,
        workflow: WorkflowConfig,
        source: Arc<dyn TaskSource>,
        sink: Arc<dyn TaskSink>,
        store: Arc<dyn StateStore>,
        router: Arc<dyn BackendRouter>,
        repo: Arc<dyn RepoProvider>,
        events_tx: mpsc::Sender<OrchestratorEvent>,
    ) -> Self {
        Self {
            config,
            workflow,
            source,
            sink,
            store,
            router,
            repo,
            events_tx,
            synthesis_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Return true when this task is goal-mode and was consumed by the goal path.
    pub async fn maybe_handle_dispatch_ready(&self, task_id: &TaskId) -> anyhow::Result<bool> {
        let Some(spec) = self.load_spec(task_id).await? else {
            return Ok(false);
        };

        if !spec.is_goal_mode() {
            return Ok(false);
        }

        if !self.workflow.goals.enabled {
            tracing::warn!(
                task_id = %task_id,
                "Goal-mode task skipped because goals.enabled is false"
            );
            self.sink
                .mark_stuck(
                    task_id.as_str(),
                    "mode:goal task requires goals.enabled = true",
                )
                .await?;
            return Ok(true);
        }

        let goal = self.ensure_goal_record(&spec).await?;
        let _ = self
            .events_tx
            .send(OrchestratorEvent::GoalReady {
                goal_id: goal.goal_id.clone(),
                task_id: task_id.clone(),
                at: chrono::Utc::now(),
            })
            .await;
        self.plan_and_launch(goal, spec).await?;
        Ok(true)
    }

    pub async fn handle_research_run_completed(&self, worker_run_id: &RunId) -> anyhow::Result<()> {
        let Some(mut research_run) = self.store.research_run_for_worker(worker_run_id).await?
        else {
            return Ok(());
        };

        let artifact = self
            .report_artifact_for(&research_run, worker_run_id)
            .await?;
        research_run.status = ResearchRunStatus::Completed;
        research_run.report_artifact = Some(artifact.clone());
        self.store.upsert_research_run(&research_run).await?;

        let _ = self
            .events_tx
            .send(OrchestratorEvent::ResearchRunCompleted {
                goal_id: research_run.goal_id.clone(),
                run_id: worker_run_id.clone(),
                report_artifact: artifact,
                at: chrono::Utc::now(),
            })
            .await;

        self.synthesize_if_ready(&research_run.goal_id).await
    }

    pub async fn handle_research_run_failed(
        &self,
        worker_run_id: &RunId,
        reason: &str,
    ) -> anyhow::Result<()> {
        let Some(mut research_run) = self.store.research_run_for_worker(worker_run_id).await?
        else {
            return Ok(());
        };

        research_run.status = ResearchRunStatus::Failed;
        self.store.upsert_research_run(&research_run).await?;

        if let Some(mut goal) = self.store.get_goal(&research_run.goal_id).await? {
            goal.status = GoalStatus::Failed;
            goal.touch();
            self.store.upsert_goal(&goal).await?;
        }

        let _ = self
            .events_tx
            .send(OrchestratorEvent::GoalFailed {
                goal_id: research_run.goal_id,
                reason: reason.to_string(),
                at: chrono::Utc::now(),
            })
            .await;

        Ok(())
    }

    async fn load_spec(&self, task_id: &TaskId) -> anyhow::Result<Option<TaskSpec>> {
        if let Some(record) = self.store.get_task(task_id).await? {
            return Ok(Some(record.spec));
        }
        Ok(self.source.fetch_by_id(task_id.as_str()).await?)
    }

    async fn ensure_goal_record(&self, spec: &TaskSpec) -> anyhow::Result<GoalRecord> {
        if let Some(existing) = self.store.goal_for_task(&spec.id).await? {
            return Ok(existing);
        }

        let mut task_record = TaskRecord::new(spec.clone());
        task_record = apply_transition(&task_record, Transition::MarkReady)?;
        task_record = apply_transition(&task_record, Transition::BeginDispatching)?;
        self.store.upsert_task(&task_record).await?;

        let objective = if spec.context.trim().is_empty() {
            spec.acceptance_criteria.clone()
        } else {
            spec.context.clone()
        };
        let goal = GoalRecord::new(
            GoalId::new_v4(),
            spec.id.clone(),
            spec.title.clone(),
            objective,
            self.workflow.goals.max_research_rounds,
        );
        self.store.upsert_goal(&goal).await?;
        Ok(goal)
    }

    async fn plan_and_launch(&self, mut goal: GoalRecord, spec: TaskSpec) -> anyhow::Result<()> {
        if goal.status != GoalStatus::Planning {
            return Ok(());
        }

        let variants = self
            .plan_research_variants_with_manager(&goal, &spec)
            .await?;
        let _ = self
            .events_tx
            .send(OrchestratorEvent::GoalPlanned {
                goal_id: goal.goal_id.clone(),
                variants: variants.clone(),
                at: chrono::Utc::now(),
            })
            .await;

        goal.status = GoalStatus::Researching;
        goal.research_round += 1;
        goal.touch();
        self.store.upsert_goal(&goal).await?;

        let backend_kind = self
            .workflow
            .goals
            .default_backend
            .clone()
            .unwrap_or_else(|| self.workflow.execution.backend.clone());
        let backend = self.router.backend(&backend_kind);
        let model = spec
            .model_override
            .clone()
            .unwrap_or_else(|| self.workflow.models.worker.clone());

        let mut first_run_id = None;
        for (index, variant) in variants.iter().enumerate() {
            let child_task_id = TaskId::new(format!(
                "{}-research-{}",
                spec.id.as_str(),
                sanitize_variant(variant)
            ));
            let worker_run_id = RunId::new_v4();
            first_run_id.get_or_insert(worker_run_id.clone());
            let report_artifact = research_report_artifact(&goal.goal_id, &worker_run_id);
            let prompt = research_prompt(
                &goal,
                &spec,
                variant,
                &worker_run_id,
                &child_task_id,
                &report_artifact,
            );

            let attempt = u32::try_from(index + 1)?;
            let mut task_run = TaskRun::new(
                worker_run_id.clone(),
                child_task_id.clone(),
                attempt,
                backend_kind.clone(),
            );
            let callback_token = if backend.is_local() {
                None
            } else {
                let token = uuid::Uuid::new_v4().to_string();
                task_run.callback_token_hash = Some(sha256_hex(token.as_bytes()));
                Some(token)
            };
            let remote_branch = if backend.is_local() {
                None
            } else {
                let branch = format!(
                    "goal/{}-{}-{}",
                    spec.id.as_str(),
                    sanitize_variant(variant),
                    worker_run_id.as_str()
                );
                let github_token =
                    std::env::var(&self.workflow.execution.github_token_env).unwrap_or_default();
                self.repo
                    .push_branch(&self.config.workspace_root, &branch, &github_token)
                    .await?;
                task_run.remote_branch = Some(branch.clone());
                Some(branch)
            };
            let callback_url = self
                .workflow
                .execution
                .callback_base_url
                .as_deref()
                .map(|base| format!("{base}/api/worker/callback"));

            self.store.upsert_run(&task_run).await?;

            let mut research_run = ResearchRun {
                research_run_id: RunId::new_v4(),
                goal_id: goal.goal_id.clone(),
                source_task_id: spec.id.clone(),
                child_task_id: child_task_id.clone(),
                variant_name: variant.clone(),
                prompt: prompt.clone(),
                backend: backend_kind.clone(),
                worker_run_id: worker_run_id.clone(),
                status: ResearchRunStatus::Running,
                report_artifact: Some(report_artifact),
            };
            self.store.upsert_research_run(&research_run).await?;

            let mut worker = self.workflow.worker_runner_config();
            worker.extensions = self.workflow.goals.worker_extensions.clone();
            for extension in spec.requested_worker_extensions() {
                if !worker.extensions.contains(&extension) {
                    worker.extensions.push(extension);
                }
            }

            let req = LaunchRequest {
                run_id: worker_run_id.as_str().to_string(),
                task_id: child_task_id.as_str().to_string(),
                attempt: task_run.attempt,
                product: self.config.product.clone(),
                prompt: prompt.clone(),
                model: model.clone(),
                worker,
                workspace_root: self.config.workspace_root.clone(),
                remote_branch,
                callback_url,
                callback_token,
                github_repo: (!self.workflow.github_repo.is_empty())
                    .then(|| self.workflow.github_repo.clone()),
                github_token: std::env::var(&self.workflow.execution.github_token_env).ok(),
                after_create_hook: self.workflow.hooks.after_create.clone(),
                before_run_hook: self.workflow.hooks.before_run.clone(),
                after_run_hook: self.workflow.hooks.after_run.clone(),
            };

            let launched = match backend.launch(req).await {
                Ok(launched) => launched,
                Err(e) => {
                    let reason = format!("backend launch failed: {e}");
                    let failed_run = apply_run_transition(
                        &task_run,
                        RunTransition::FailureSignaled {
                            reason: reason.clone(),
                        },
                    )?;
                    self.store.upsert_run(&failed_run).await?;
                    research_run.status = ResearchRunStatus::Failed;
                    self.store.upsert_research_run(&research_run).await?;
                    self.fail_goal_launch(&goal, &spec, &reason).await?;
                    return Err(e.into());
                }
            };
            task_run = self
                .store
                .get_run(&worker_run_id)
                .await?
                .unwrap_or(task_run);
            task_run.handle = Some(launched.handle);
            task_run.worktree_path = launched
                .worktree_path
                .map(|path| path.to_string_lossy().to_string());
            if let Some(branch) = launched.remote_branch {
                task_run.remote_branch = Some(branch);
            }
            self.store.upsert_run(&task_run).await?;

            let _ = self
                .events_tx
                .send(OrchestratorEvent::ResearchRunLaunched {
                    goal_id: goal.goal_id.clone(),
                    run_id: worker_run_id,
                    at: chrono::Utc::now(),
                })
                .await;
        }

        if let Some(run_id) = first_run_id {
            if let Some(mut record) = self.store.get_task(&spec.id).await? {
                if record.status == TaskStatus::Dispatching {
                    record = apply_transition(&record, Transition::RunLaunched { run_id })?;
                    self.store.upsert_task(&record).await?;
                }
            }
            if let Err(e) = self.sink.mark_in_progress(spec.id.as_str()).await {
                tracing::warn!(task_id = %spec.id, "Failed to update goal task in Beads: {e}");
            }
        }

        Ok(())
    }

    async fn fail_goal_launch(
        &self,
        goal: &GoalRecord,
        spec: &TaskSpec,
        reason: &str,
    ) -> anyhow::Result<()> {
        let mut failed_goal = goal.clone();
        failed_goal.status = GoalStatus::Failed;
        failed_goal.touch();
        self.store.upsert_goal(&failed_goal).await?;

        if let Some(record) = self.store.get_task(&spec.id).await? {
            if record.status == TaskStatus::Dispatching {
                let failed = apply_transition(
                    &record,
                    Transition::DispatchFailed {
                        reason: reason.to_string(),
                    },
                )?;
                self.store.upsert_task(&failed).await?;
            }
        }

        if let Err(e) = self.sink.mark_stuck(spec.id.as_str(), reason).await {
            tracing::warn!(task_id = %spec.id, "Failed to update failed goal task in Beads: {e}");
        }

        let _ = self
            .events_tx
            .send(OrchestratorEvent::GoalFailed {
                goal_id: goal.goal_id.clone(),
                reason: reason.to_string(),
                at: chrono::Utc::now(),
            })
            .await;

        Ok(())
    }

    async fn report_artifact_for(
        &self,
        research_run: &ResearchRun,
        worker_run_id: &RunId,
    ) -> anyhow::Result<String> {
        let Some(run) = self.store.get_run(worker_run_id).await? else {
            anyhow::bail!("worker run {worker_run_id} not found");
        };
        let relative = research_run
            .report_artifact
            .clone()
            .unwrap_or_else(|| research_report_artifact(&research_run.goal_id, worker_run_id));
        validate_report_artifact_path(&relative)?;
        if let Some(worktree) = run.worktree_path.as_deref() {
            let path = Path::new(worktree).join(&relative);
            parse_research_report(&path, &research_run.goal_id, worker_run_id)?;
        }
        Ok(relative)
    }

    async fn synthesize_if_ready(&self, goal_id: &GoalId) -> anyhow::Result<()> {
        let _guard = self.synthesis_lock.lock().await;
        let runs = self.store.research_runs_for_goal(goal_id).await?;
        if runs.is_empty()
            || runs
                .iter()
                .any(|run| run.status != ResearchRunStatus::Completed)
        {
            return Ok(());
        }

        let Some(mut goal) = self.store.get_goal(goal_id).await? else {
            return Ok(());
        };
        if goal.status != GoalStatus::Researching {
            return Ok(());
        }
        goal.status = GoalStatus::Synthesizing;
        goal.touch();
        self.store.upsert_goal(&goal).await?;
        let _ = self
            .events_tx
            .send(OrchestratorEvent::GoalSynthesisReady {
                goal_id: goal_id.clone(),
                at: chrono::Utc::now(),
            })
            .await;

        let reports = self.load_reports(&runs).await?;
        let decision = self
            .synthesize_reports_with_manager(&goal, &reports)
            .await?;
        self.apply_decision(goal, decision).await
    }

    async fn synthesize_reports_with_manager(
        &self,
        goal: &GoalRecord,
        reports: &[ResearchReport],
    ) -> anyhow::Result<GoalSynthesisDecision> {
        let api_key = std::env::var("OPENROUTER_API_KEY")
            .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
            .unwrap_or_default();
        if api_key.is_empty() {
            tracing::warn!(
                goal_id = %goal.goal_id,
                "Manager synthesis API key not set; using deterministic synthesis fallback"
            );
            return Ok(synthesize_reports(reports));
        }

        let api_base = std::env::var("MANAGER_API_BASE")
            .unwrap_or_else(|_| "https://openrouter.ai/api/v1".into());
        let prompt = format!(
            r#"Synthesize these Thala goal-mode research reports.

Goal:
{}

Original objective:
{}

Available actions:
- done
- ask_human
- create_tasks
- research_more
- failed

Respond with JSON only matching:
{{
  "decision": "done|ask_human|create_tasks|research_more|failed",
  "summary": "string",
  "human_question": "string|null",
  "follow_up_tasks": [{{"title":"string","acceptance_criteria":"string","context":"string"}}],
  "reason": "string"
}}

Reports:
{}
"#,
            goal.title,
            goal.objective,
            serde_json::to_string_pretty(reports)?
        );

        let response = reqwest::Client::new()
            .post(format!("{api_base}/chat/completions"))
            .bearer_auth(api_key)
            .json(&serde_json::json!({
                "model": self.workflow.models.manager,
                "messages": [{"role": "user", "content": prompt}],
                "max_tokens": 1200,
                "temperature": 0.0,
            }))
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "manager synthesis API returned {status}: {}",
                truncate_for_error(&text)
            );
        }

        let data: Value = response.json().await?;
        let content = extract_message_content(&data)?;
        let json = extract_json_object(&content)?;
        Ok(serde_json::from_str(json)?)
    }

    async fn plan_research_variants_with_manager(
        &self,
        goal: &GoalRecord,
        spec: &TaskSpec,
    ) -> anyhow::Result<Vec<String>> {
        let fallback = research_variants(self.workflow.goals.max_parallel_research_runs);
        let api_key = std::env::var("OPENROUTER_API_KEY")
            .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
            .unwrap_or_default();
        if api_key.is_empty() {
            tracing::warn!(
                goal_id = %goal.goal_id,
                "Manager planning API key not set; using deterministic research variants"
            );
            return Ok(fallback);
        }

        let api_base = std::env::var("MANAGER_API_BASE")
            .unwrap_or_else(|_| "https://openrouter.ai/api/v1".into());
        let prompt = format!(
            r#"Create a small research fanout plan for this Thala goal.

Goal:
{}

Objective:
{}

Acceptance criteria:
{}

Return JSON only:
{{
  "variants": ["short-kebab-case-name"]
}}

Limit variants to at most {}. Use distinct approaches.
"#,
            goal.title,
            goal.objective,
            spec.acceptance_criteria,
            self.workflow.goals.max_parallel_research_runs
        );

        let response = reqwest::Client::new()
            .post(format!("{api_base}/chat/completions"))
            .bearer_auth(api_key)
            .json(&serde_json::json!({
                "model": self.workflow.models.manager,
                "messages": [{"role": "user", "content": prompt}],
                "max_tokens": 512,
                "temperature": 0.0,
            }))
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "manager planning API returned {status}: {}",
                truncate_for_error(&text)
            );
        }

        let data: Value = response.json().await?;
        let content = extract_message_content(&data)?;
        let json = extract_json_object(&content)?;
        let plan: ResearchPlan = serde_json::from_str(json)?;
        let variants = plan
            .variants
            .into_iter()
            .filter(|variant| !variant.trim().is_empty())
            .take(self.workflow.goals.max_parallel_research_runs.max(1))
            .collect::<Vec<_>>();
        if variants.is_empty() {
            Ok(fallback)
        } else {
            Ok(variants)
        }
    }

    async fn load_reports(&self, runs: &[ResearchRun]) -> anyhow::Result<Vec<ResearchReport>> {
        let mut reports = Vec::new();
        for run in runs {
            let Some(artifact) = run.report_artifact.as_deref() else {
                anyhow::bail!("research run {} has no report artifact", run.worker_run_id);
            };
            validate_report_artifact_path(artifact)?;
            if let Some(task_run) = self.store.get_run(&run.worker_run_id).await? {
                if let Some(worktree) = task_run.worktree_path.as_deref() {
                    reports.push(parse_research_report(
                        &Path::new(worktree).join(artifact),
                        &run.goal_id,
                        &run.worker_run_id,
                    )?);
                    continue;
                }
                if let Some(branch) = task_run.remote_branch.as_deref() {
                    reports.push(
                        self.read_remote_report(branch, artifact, &run.goal_id, &run.worker_run_id)
                            .await?,
                    );
                    continue;
                }
            }
            reports.push(synthetic_remote_report(run, artifact));
        }
        Ok(reports)
    }

    async fn read_remote_report(
        &self,
        branch: &str,
        artifact: &str,
        goal_id: &GoalId,
        run_id: &RunId,
    ) -> anyhow::Result<ResearchReport> {
        let fetch = tokio::process::Command::new("git")
            .arg("-C")
            .arg(&self.config.workspace_root)
            .args(["fetch", "origin", branch])
            .output()
            .await?;
        if !fetch.status.success() {
            anyhow::bail!(
                "failed to fetch remote research branch {branch}: {}",
                String::from_utf8_lossy(&fetch.stderr).trim()
            );
        }

        let object = format!("origin/{branch}:{artifact}");
        let show = tokio::process::Command::new("git")
            .arg("-C")
            .arg(&self.config.workspace_root)
            .args(["show", &object])
            .output()
            .await?;
        if !show.status.success() {
            anyhow::bail!(
                "failed to read research artifact {artifact} from {branch}: {}",
                String::from_utf8_lossy(&show.stderr).trim()
            );
        }

        let raw = String::from_utf8(show.stdout)?;
        parse_research_report_json(&raw, goal_id, run_id)
    }

    async fn apply_decision(
        &self,
        mut goal: GoalRecord,
        decision: GoalSynthesisDecision,
    ) -> anyhow::Result<()> {
        match decision.decision {
            GoalDecisionKind::Done => {
                goal.status = GoalStatus::Succeeded;
                goal.touch();
                self.store.upsert_goal(&goal).await?;
                self.sink
                    .append_context(goal.source_task_id.as_ref(), &decision.summary)
                    .await?;
                let _ = self.sink.mark_done(goal.source_task_id.as_ref(), 0).await;
                let _ = self
                    .events_tx
                    .send(OrchestratorEvent::GoalCompleted {
                        goal_id: goal.goal_id,
                        at: chrono::Utc::now(),
                    })
                    .await;
            }
            GoalDecisionKind::CreateTasks
                if self.workflow.goals.require_human_before_followup_tasks =>
            {
                goal.status = GoalStatus::WaitingForHuman;
                goal.touch();
                self.store.upsert_goal(&goal).await?;
                let _ = self
                    .events_tx
                    .send(OrchestratorEvent::GoalActionRequired {
                        goal_id: goal.goal_id,
                        reason: "follow-up task creation requires human approval".into(),
                        at: chrono::Utc::now(),
                    })
                    .await;
            }
            GoalDecisionKind::CreateTasks => {
                goal.status = GoalStatus::CreatingFollowups;
                goal.touch();
                self.store.upsert_goal(&goal).await?;
                for task in decision.follow_up_tasks {
                    self.sink
                        .create_task(NewTaskRequest {
                            title: task.title,
                            acceptance_criteria: task.acceptance_criteria,
                            context: task.context,
                            priority: None,
                            labels: vec!["generated-by:thala-goal".into()],
                            submitted_by: "thala:goal".into(),
                            always_human_review: false,
                        })
                        .await?;
                }
                goal.status = GoalStatus::Succeeded;
                goal.touch();
                self.store.upsert_goal(&goal).await?;
            }
            GoalDecisionKind::ResearchMore if goal.research_round < goal.max_rounds => {
                goal.status = GoalStatus::Planning;
                goal.touch();
                self.store.upsert_goal(&goal).await?;
                if let Some(spec) = self.load_spec(&goal.source_task_id).await? {
                    self.plan_and_launch(goal, spec).await?;
                }
            }
            GoalDecisionKind::AskHuman | GoalDecisionKind::ResearchMore => {
                goal.status = GoalStatus::WaitingForHuman;
                goal.touch();
                self.store.upsert_goal(&goal).await?;
                let _ = self
                    .events_tx
                    .send(OrchestratorEvent::GoalActionRequired {
                        goal_id: goal.goal_id,
                        reason: decision.reason,
                        at: chrono::Utc::now(),
                    })
                    .await;
            }
            GoalDecisionKind::Failed => {
                goal.status = GoalStatus::Failed;
                goal.touch();
                self.store.upsert_goal(&goal).await?;
                let _ = self
                    .events_tx
                    .send(OrchestratorEvent::GoalFailed {
                        goal_id: goal.goal_id,
                        reason: decision.reason,
                        at: chrono::Utc::now(),
                    })
                    .await;
            }
        }
        Ok(())
    }
}

fn research_variants(max: usize) -> Vec<String> {
    ["codebase-map", "risk-scan", "implementation-options"]
        .into_iter()
        .take(max.max(1))
        .map(str::to_string)
        .collect()
}

fn research_prompt(
    goal: &GoalRecord,
    spec: &TaskSpec,
    variant: &str,
    worker_run_id: &RunId,
    child_task_id: &TaskId,
    report_artifact: &str,
) -> String {
    format!(
        r"You are a research worker for Thala goal mode.

Goal ID: {goal_id}
Run ID: {run_id}
Source task: {source_task}
Goal title: {title}
Variant: {variant}

Objective:
{objective}

Acceptance criteria:
{acceptance_criteria}

Produce a structured JSON report at `{report_artifact}` with:
- goal_id
- run_id
- variant
- summary
- findings: claim, evidence, source, confidence
- recommendation
- risks
- follow_up_questions
- suggested_tasks: title, acceptance_criteria, context

Do not create a PR. Write DONE to `.thala/signals/{child_task_id}.signal` when the report is complete.
",
        goal_id = goal.goal_id,
        run_id = worker_run_id,
        source_task = spec.id,
        title = goal.title,
        objective = goal.objective,
        acceptance_criteria = spec.acceptance_criteria,
    )
}

fn research_report_artifact(goal_id: &GoalId, run_id: &RunId) -> String {
    format!(".thala/research/{goal_id}/{run_id}.json")
}

fn parse_research_report(
    path: &Path,
    goal_id: &GoalId,
    run_id: &RunId,
) -> anyhow::Result<ResearchReport> {
    let raw = std::fs::read_to_string(path)?;
    parse_research_report_json(&raw, goal_id, run_id)
}

fn parse_research_report_json(
    raw: &str,
    goal_id: &GoalId,
    run_id: &RunId,
) -> anyhow::Result<ResearchReport> {
    let report: ResearchReport = serde_json::from_str(raw)?;
    if report.goal_id != goal_id.as_str() || report.run_id != run_id.as_str() {
        anyhow::bail!("research report IDs do not match expected goal/run");
    }
    Ok(report)
}

fn validate_report_artifact_path(path: &str) -> anyhow::Result<()> {
    let path = Path::new(path);
    if path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        anyhow::bail!("research report artifact path must be relative and stay inside workspace");
    }
    Ok(())
}

fn synthesize_reports(reports: &[ResearchReport]) -> GoalSynthesisDecision {
    let suggested_tasks = reports
        .iter()
        .flat_map(|report| report.suggested_tasks.clone())
        .collect::<Vec<_>>();
    if !suggested_tasks.is_empty() {
        return GoalSynthesisDecision {
            decision: GoalDecisionKind::CreateTasks,
            summary: reports
                .iter()
                .map(|report| report.summary.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
            human_question: Some("Approve creation of synthesized follow-up tasks?".into()),
            follow_up_tasks: suggested_tasks,
            reason: "research reports suggested implementation tasks".into(),
        };
    }

    GoalSynthesisDecision {
        decision: GoalDecisionKind::Done,
        summary: reports
            .iter()
            .map(|report| report.summary.as_str())
            .collect::<Vec<_>>()
            .join("\n\n"),
        human_question: None,
        follow_up_tasks: Vec::new(),
        reason: "research completed without follow-up tasks".into(),
    }
}

fn extract_message_content(data: &Value) -> anyhow::Result<String> {
    let Some(content) = data.pointer("/choices/0/message/content") else {
        anyhow::bail!("manager synthesis response did not include choices[0].message.content");
    };

    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| part.get("content").and_then(Value::as_str))
            })
            .collect::<String>(),
        _ => anyhow::bail!("manager synthesis response content had unexpected type"),
    };

    if text.trim().is_empty() {
        anyhow::bail!("manager synthesis response was empty");
    }

    Ok(text)
}

fn extract_json_object(content: &str) -> anyhow::Result<&str> {
    let trimmed = content.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Ok(trimmed);
    }

    let without_fence = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|inner| inner.strip_suffix("```"))
        .map(str::trim);

    if let Some(json) = without_fence {
        if json.starts_with('{') && json.ends_with('}') {
            return Ok(json);
        }
    }

    let Some(start) = trimmed.find('{') else {
        anyhow::bail!(
            "manager synthesis response did not contain a JSON object. Raw: {}",
            truncate_for_error(content)
        );
    };
    let Some(end) = trimmed.rfind('}') else {
        anyhow::bail!(
            "manager synthesis response did not contain a complete JSON object. Raw: {}",
            truncate_for_error(content)
        );
    };

    if start >= end {
        anyhow::bail!(
            "manager synthesis response did not contain a valid JSON object. Raw: {}",
            truncate_for_error(content)
        );
    }

    Ok(&trimmed[start..=end])
}

fn truncate_for_error(text: &str) -> String {
    const MAX_CHARS: usize = 1_000;

    let mut chars = text.chars();
    let truncated = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

fn synthetic_remote_report(run: &ResearchRun, artifact: &str) -> ResearchReport {
    ResearchReport {
        goal_id: run.goal_id.as_str().to_string(),
        run_id: run.worker_run_id.as_str().to_string(),
        variant: run.variant_name.clone(),
        summary: format!("Research report available at {artifact}"),
        findings: Vec::new(),
        recommendation: String::new(),
        risks: Vec::new(),
        follow_up_questions: Vec::new(),
        suggested_tasks: Vec::new(),
    }
}

fn sanitize_variant(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
