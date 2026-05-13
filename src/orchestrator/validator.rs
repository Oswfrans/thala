//! Validator coordinator — runs validators and drives post-validation transitions.
//!
//! Consumes RunCompleted events and coordinates the validation pipeline:
//!   1. Run ReviewAi validator against the diff.
//!   2. If review passes: create PR → task enters Validating.
//!   3. If review fails: inject feedback, check retry budget, re-dispatch or fail.
//!   4. Once PR exists: CiValidator polls for check status.
//!   5. On CI pass + auto_merge enabled: trigger merge.
//!   6. On CI pass + human required: emit InteractionRequest (ApprovalRequired).
//!   7. On PR merged: transition task to Succeeded, write back to Beads.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::sleep;

use crate::core::events::OrchestratorEvent;
use crate::core::ids::{RunId, TaskId};
use crate::core::interaction::{InteractionAction, InteractionRequest, InteractionRequestKind};
use crate::core::run::TaskRun;
use crate::core::task::TaskStatus;
use crate::core::transitions::{apply_transition, Transition};
use crate::core::workflow::WorkflowConfig;
use crate::ports::interaction::InteractionLayer;
use crate::ports::repo::{CiStatus, RepoProvider};
use crate::ports::state_store::StateStore;
use crate::ports::task_sink::TaskSink;
use crate::ports::validator::Validator;

// ── ValidatorCoordinator ──────────────────────────────────────────────────────

pub struct ValidatorCoordinator {
    workflow: WorkflowConfig,
    review_ai: Arc<dyn Validator>,
    repo: Arc<dyn RepoProvider>,
    store: Arc<dyn StateStore>,
    sink: Arc<dyn TaskSink>,
    interaction_layers: Vec<Arc<dyn InteractionLayer>>,
    events_tx: tokio::sync::mpsc::Sender<OrchestratorEvent>,
}

impl ValidatorCoordinator {
    pub fn new(
        workflow: WorkflowConfig,
        review_ai: Arc<dyn Validator>,
        repo: Arc<dyn RepoProvider>,
        store: Arc<dyn StateStore>,
        sink: Arc<dyn TaskSink>,
        interaction_layers: Vec<Arc<dyn InteractionLayer>>,
        events_tx: tokio::sync::mpsc::Sender<OrchestratorEvent>,
    ) -> Self {
        Self {
            workflow,
            review_ai,
            repo,
            store,
            sink,
            interaction_layers,
            events_tx,
        }
    }

    /// Validation polling loop — mirrors the run() pattern of other subsystems.
    ///
    /// Each tick loads Validating tasks from the StateStore and advances CI checks
    /// and PR-merge detection. Runs until the process exits.
    pub async fn run(self: Arc<Self>, poll_interval: Duration) {
        tracing::info!(
            poll_interval_secs = poll_interval.as_secs(),
            "Validator loop starting"
        );
        loop {
            match self.store.active_tasks().await {
                Ok(tasks) => {
                    for task in tasks {
                        if task.status != TaskStatus::Validating {
                            continue;
                        }
                        let Some(run_id) = task.active_run_id.clone() else {
                            continue;
                        };
                        if let Err(e) = self.check_ci(&task.spec.id, &run_id).await {
                            tracing::warn!(
                                task_id = %task.spec.id,
                                run_id = %run_id,
                                "CI check failed: {e}"
                            );
                        }
                        if let Err(e) = self.check_pr_merged(&task.spec.id, &run_id).await {
                            tracing::warn!(
                                task_id = %task.spec.id,
                                run_id = %run_id,
                                "PR merge check failed: {e}"
                            );
                        }
                    }
                }
                Err(e) => tracing::warn!("Validation loop failed to load active tasks: {e}"),
            }
            sleep(poll_interval).await;
        }
    }

    /// Handle a RunCompleted event — run review AI and progress the pipeline.
    pub async fn handle_run_completed(
        &self,
        task_id: &TaskId,
        run_id: &RunId,
    ) -> anyhow::Result<()> {
        let Some(run) = self.store.get_run(run_id).await? else {
            tracing::warn!(run_id = %run_id, "Run not found for validation");
            return Ok(());
        };

        let Some(mut record) = self.store.get_task(task_id).await? else {
            tracing::warn!(task_id = %task_id, "Task not found for validation");
            return Ok(());
        };

        if record.status == TaskStatus::Running {
            record = apply_transition(&record, Transition::RunCompleted)?;
            self.store.upsert_task(&record).await?;
        } else if record.status != TaskStatus::Validating {
            tracing::warn!(
                task_id = %task_id,
                status = %record.status.as_str(),
                "RunCompleted received for task that is not ready for validation"
            );
            return Ok(());
        }

        // Step 1: Review AI.
        let review_outcome = match self.review_ai.validate(&run, &record.spec).await {
            Ok(outcome) => outcome,
            Err(e) => {
                return self
                    .handle_validation_infrastructure_failure(
                        &run,
                        &mut record,
                        format!("Review AI validation failed: {e}"),
                    )
                    .await;
            }
        };

        let _ = self
            .events_tx
            .send(OrchestratorEvent::ValidationResult {
                task_id: task_id.clone(),
                run_id: run_id.clone(),
                outcome: review_outcome.clone(),
                at: chrono::Utc::now(),
            })
            .await;

        if !review_outcome.passed {
            return self
                .handle_review_failure(&run, &mut record, review_outcome.detail.as_deref())
                .await;
        }

        // Step 2: Create PR.
        let pr_title = format!("[Thala] {} — {}", record.spec.id, record.spec.title);
        let pr_body = format!(
            "Automated by Thala\n\n**Acceptance Criteria:**\n{}\n\n**Review AI:** ✅",
            record.spec.acceptance_criteria
        );

        let branch = run
            .remote_branch
            .clone()
            .unwrap_or_else(|| format!("task/{}", task_id.as_str()));

        if let Some(worktree_path) = run.worktree_path.as_deref() {
            let github_token =
                std::env::var(&self.workflow.execution.github_token_env).unwrap_or_default();
            if let Err(e) = self
                .repo
                .push_branch(std::path::Path::new(worktree_path), &branch, &github_token)
                .await
            {
                return self
                    .handle_validation_infrastructure_failure(
                        &run,
                        &mut record,
                        format!("Branch push failed: {e}"),
                    )
                    .await;
            }
        }

        let (pr_number, pr_url) = match self.repo.create_pr(&branch, &pr_title, &pr_body).await {
            Ok(pr) => pr,
            Err(e) => {
                return self
                    .handle_validation_infrastructure_failure(
                        &run,
                        &mut record,
                        format!("PR creation failed: {e}"),
                    )
                    .await;
            }
        };

        // Update run with PR info.
        let mut updated_run = run.clone();
        updated_run.pr_number = Some(pr_number);
        updated_run.pr_url = Some(pr_url.clone());
        self.store.upsert_run(&updated_run).await?;

        tracing::info!(
            task_id = %task_id,
            pr_number,
            pr_url = %pr_url,
            "PR created, entering CI validation"
        );

        // CI/merge decisions are handled by the validation polling loop.
        Ok(())
    }

    /// Check CI status for a Validating task and advance if ready.
    pub async fn check_ci(&self, task_id: &TaskId, run_id: &RunId) -> anyhow::Result<()> {
        let Some(run) = self.store.get_run(run_id).await? else {
            return Ok(());
        };

        let Some(pr_number) = run.pr_number else {
            return Ok(());
        };

        let Some(record) = self.store.get_task(task_id).await? else {
            return Ok(());
        };

        let ci_status = self.repo.pr_ci_status(pr_number).await?;

        // Get the diff to check protected paths (local backend only).
        let diff = if let Some(wt_path) = &run.worktree_path {
            self.repo
                .get_diff(std::path::Path::new(wt_path))
                .await
                .unwrap_or_default()
        } else {
            String::new()
        };

        match ci_status {
            CiStatus::Passing => {
                tracing::info!(task_id = %task_id, pr_number, "CI passing");

                if self.workflow.merge.auto_merge
                    && !record.spec.always_human_review
                    && !record_requires_human_approval(&run, &self.workflow, &diff)
                {
                    self.trigger_merge(task_id, run_id, pr_number).await?;
                } else {
                    // Auto-merge blocked by policy or protected paths — request human approval.
                    if let Some(pr_url) = run.pr_url.as_deref() {
                        self.request_approval(task_id, run_id, pr_number, pr_url)
                            .await?;
                    }
                }
            }
            CiStatus::Failing { ref failing_checks } => {
                tracing::warn!(
                    task_id = %task_id,
                    pr_number,
                    ?failing_checks,
                    "CI failing"
                );

                // Check retry budget.
                if record.attempt < self.workflow.retry.max_attempts {
                    tracing::info!(
                        task_id = %task_id,
                        attempt = record.attempt,
                        max = self.workflow.retry.max_attempts,
                        "CI failing — re-dispatching within retry budget"
                    );
                    let updated = crate::core::transitions::apply_transition(
                        &record,
                        crate::core::transitions::Transition::ValidationFailedRetry {
                            reason: format!("CI failing: {}", failing_checks.join(", ")),
                        },
                    )?;
                    self.store.upsert_task(&updated).await?;
                    let _ = self
                        .events_tx
                        .send(OrchestratorEvent::dispatch_ready(task_id.clone()))
                        .await;
                } else {
                    tracing::warn!(
                        task_id = %task_id,
                        attempt = record.attempt,
                        "CI failing — max retries exceeded, marking failed"
                    );
                    let updated = crate::core::transitions::apply_transition(
                        &record,
                        crate::core::transitions::Transition::ValidationFailedTerminal {
                            reason: format!(
                                "CI failing after {} attempts: {}",
                                record.attempt,
                                failing_checks.join(", ")
                            ),
                        },
                    )?;
                    self.store.upsert_task(&updated).await?;

                    if let Err(e) = self
                        .sink
                        .mark_stuck(
                            task_id.as_str(),
                            &format!(
                                "CI failing after max retries: {}",
                                failing_checks.join(", ")
                            ),
                        )
                        .await
                    {
                        tracing::warn!(task_id = %task_id, "Failed to mark task stuck in Beads: {e}");
                    }
                }
            }
            CiStatus::Pending | CiStatus::Unknown => {
                tracing::debug!(task_id = %task_id, pr_number, "CI pending");
            }
        }

        Ok(())
    }

    /// Check whether a PR has been merged and advance the task to Succeeded.
    pub async fn check_pr_merged(&self, task_id: &TaskId, run_id: &RunId) -> anyhow::Result<()> {
        let Some(run) = self.store.get_run(run_id).await? else {
            return Ok(());
        };

        let Some(pr_number) = run.pr_number else {
            return Ok(());
        };

        if self.repo.pr_is_merged(pr_number).await? {
            tracing::info!(task_id = %task_id, pr_number, "PR merged — task succeeded");
            self.complete_task(task_id, pr_number).await?;
        }

        Ok(())
    }

    async fn handle_review_failure(
        &self,
        run: &TaskRun,
        record: &mut crate::core::task::TaskRecord,
        feedback: Option<&str>,
    ) -> anyhow::Result<()> {
        let max_cycles = self.workflow.models.max_review_cycles;

        if run.review_cycle >= max_cycles {
            tracing::warn!(
                task_id = %record.spec.id,
                review_cycle = run.review_cycle,
                max_cycles,
                "Max review cycles reached — marking failed"
            );
            *record = apply_transition(
                record,
                Transition::ValidationFailedTerminal {
                    reason: format!("Max review cycles ({max_cycles}) reached without approval"),
                },
            )?;
            self.store.upsert_task(record).await?;
        } else {
            tracing::info!(
                task_id = %record.spec.id,
                review_cycle = run.review_cycle,
                "Review rejected — injecting feedback and re-dispatching"
            );
            // Store feedback in the run record for the next dispatcher pick-up.
            let mut updated_run = run.clone();
            updated_run.review_feedback = feedback.map(ToString::to_string);
            updated_run.review_cycle += 1;
            self.store.upsert_run(&updated_run).await?;

            *record = apply_transition(
                record,
                Transition::ValidationFailedRetry {
                    reason: feedback.unwrap_or("Review rejected").to_string(),
                },
            )?;
            self.store.upsert_task(record).await?;
            let _ = self
                .events_tx
                .send(OrchestratorEvent::dispatch_ready(record.spec.id.clone()))
                .await;
        }

        Ok(())
    }

    async fn handle_validation_infrastructure_failure(
        &self,
        run: &TaskRun,
        record: &mut crate::core::task::TaskRecord,
        reason: String,
    ) -> anyhow::Result<()> {
        tracing::warn!(
            task_id = %record.spec.id,
            attempt = record.attempt,
            max_attempts = self.workflow.retry.max_attempts,
            "Validation infrastructure failure: {reason}"
        );

        if record.attempt < self.workflow.retry.max_attempts {
            let mut updated_run = run.clone();
            updated_run.review_feedback = Some(reason.clone());
            self.store.upsert_run(&updated_run).await?;

            *record = apply_transition(record, Transition::ValidationFailedRetry { reason })?;
            self.store.upsert_task(record).await?;

            let _ = self
                .events_tx
                .send(OrchestratorEvent::dispatch_ready(record.spec.id.clone()))
                .await;
        } else {
            *record = apply_transition(
                record,
                Transition::ValidationFailedTerminal {
                    reason: reason.clone(),
                },
            )?;
            self.store.upsert_task(record).await?;

            if let Err(e) = self.sink.mark_stuck(record.spec.id.as_str(), &reason).await {
                tracing::warn!(
                    task_id = %record.spec.id,
                    "Failed to mark task stuck in Beads after validation infrastructure failure: {e}"
                );
            }
        }

        Ok(())
    }

    async fn request_approval(
        &self,
        task_id: &TaskId,
        run_id: &RunId,
        pr_number: u32,
        pr_url: &str,
    ) -> anyhow::Result<()> {
        let Some(mut record) = self.store.get_task(task_id).await? else {
            return Ok(());
        };

        if matches!(record.status, crate::core::task::TaskStatus::Validating) {
            record = apply_transition(&record, Transition::RequireHumanInput)?;
            self.store.upsert_task(&record).await?;
        }

        let req = InteractionRequest::new(
            task_id.clone(),
            run_id.clone(),
            InteractionRequestKind::ApprovalRequired {
                pr_url: pr_url.to_string(),
                pr_number,
            },
            format!(
                "Approval required: {} — {}",
                record.spec.id, record.spec.title
            ),
            format!(
                "PR #{pr_number} is ready for review.\n\nAcceptance Criteria:\n{}",
                record.spec.acceptance_criteria
            ),
            vec![InteractionAction::Approve, InteractionAction::Reject],
        );

        let ticket = crate::core::interaction::InteractionTicket::new(req.clone());
        self.store.save_ticket(&ticket).await?;

        for layer in &self.interaction_layers {
            if let Err(e) = layer.send(&req).await {
                tracing::warn!(
                    channel = layer.name(),
                    "Failed to send approval request: {e}"
                );
            }
        }

        Ok(())
    }

    pub async fn handle_human_approved(
        &self,
        task_id: &TaskId,
        run_id: &RunId,
    ) -> anyhow::Result<()> {
        let Some(run) = self.store.get_run(run_id).await? else {
            return Ok(());
        };
        let Some(pr_number) = run.pr_number else {
            return Ok(());
        };
        self.trigger_merge(task_id, run_id, pr_number).await
    }

    async fn trigger_merge(
        &self,
        task_id: &TaskId,
        run_id: &RunId,
        pr_number: u32,
    ) -> anyhow::Result<()> {
        // Hard rule: "thala-core" product never auto-merges.
        if self.workflow.product == "thala-core" {
            tracing::info!(
                task_id = %task_id,
                "thala-core product — auto-merge blocked by hard rule"
            );
            self.request_approval(task_id, run_id, pr_number, "")
                .await?;
            return Ok(());
        }

        tracing::info!(task_id = %task_id, pr_number, "Triggering auto-merge");
        self.repo.merge_pr(pr_number).await?;
        self.complete_task(task_id, pr_number).await?;
        Ok(())
    }

    async fn complete_task(&self, task_id: &TaskId, pr_number: u32) -> anyhow::Result<()> {
        if let Some(mut record) = self.store.get_task(task_id).await? {
            record = apply_transition(&record, Transition::ValidationPassed)?;
            self.store.upsert_task(&record).await?;
        }

        if let Err(e) = self.sink.mark_done(task_id.as_str(), pr_number).await {
            tracing::warn!(task_id = %task_id, "Failed to write done back to Beads: {e}");
        }

        let _ = self
            .events_tx
            .send(OrchestratorEvent::TaskSyncedToBeads {
                task_id: task_id.clone(),
                beads_status: "closed".into(),
                at: chrono::Utc::now(),
            })
            .await;

        Ok(())
    }
}

fn record_requires_human_approval(run: &TaskRun, workflow: &WorkflowConfig, diff: &str) -> bool {
    if run.review_cycle > 0 || !workflow.merge.auto_merge {
        return true;
    }

    // Block auto-merge when any protected path was touched by this diff.
    if !workflow.merge.protected_paths.is_empty()
        && diff_touches_protected_path(diff, &workflow.merge.protected_paths)
    {
        tracing::info!(
            run_id = %run.run_id,
            "Diff touches a protected path — human approval required"
        );
        return true;
    }

    false
}

/// Return true if the git diff output contains changes to any protected path.
///
/// Matches against `+++ b/<path>` and `--- a/<path>` lines in the diff output.
/// Patterns are treated as globs (e.g. "auth/**", "**/migrations/**") using the
/// globset crate — plain prefixes like "auth/" also work.
fn diff_touches_protected_path(diff: &str, patterns: &[String]) -> bool {
    use globset::{Glob, GlobSetBuilder};

    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        match Glob::new(p) {
            Ok(g) => {
                builder.add(g);
            }
            Err(e) => tracing::warn!(pattern = %p, "Invalid protected_path glob: {e}"),
        }
    }
    let set = match builder.build() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("Failed to compile protected_paths glob set: {e}");
            return false;
        }
    };

    for line in diff.lines() {
        let file_path = if let Some(rest) = line.strip_prefix("+++ b/") {
            rest
        } else if let Some(rest) = line.strip_prefix("--- a/") {
            rest
        } else {
            continue;
        };

        if set.is_match(file_path) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod validation_flow_tests {
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use tempfile::TempDir;

    use super::ValidatorCoordinator;
    use crate::adapters::state::SqliteStateStore;
    use crate::core::error::ThalaError;
    use crate::core::ids::{RunId, TaskId};
    use crate::core::run::{ExecutionBackendKind, TaskRun};
    use crate::core::task::{TaskRecord, TaskSpec, TaskStatus};
    use crate::core::transitions::{
        apply_run_transition, apply_transition, RunTransition, Transition,
    };
    use crate::core::validation::{ValidationOutcome, ValidatorKind};
    use crate::core::workflow::WorkflowConfig;
    use crate::ports::repo::{CiStatus, RepoProvider};
    use crate::ports::state_store::StateStore;
    use crate::ports::task_sink::{NewTaskRequest, TaskSink};
    use crate::ports::validator::Validator;

    struct FailingValidator;

    #[async_trait]
    impl Validator for FailingValidator {
        fn kind(&self) -> ValidatorKind {
            ValidatorKind::ReviewAi
        }

        async fn validate(
            &self,
            run: &TaskRun,
            _spec: &TaskSpec,
        ) -> Result<ValidationOutcome, ThalaError> {
            Ok(ValidationOutcome::fail(
                run.run_id.clone(),
                ValidatorKind::ReviewAi,
                "Review AI rejected the diff",
                "needs fix",
            ))
        }
    }

    struct ErrorValidator;

    #[async_trait]
    impl Validator for ErrorValidator {
        fn kind(&self) -> ValidatorKind {
            ValidatorKind::ReviewAi
        }

        async fn validate(
            &self,
            _run: &TaskRun,
            _spec: &TaskSpec,
        ) -> Result<ValidationOutcome, ThalaError> {
            Err(ThalaError::Validation("review service unavailable".into()))
        }
    }

    struct PassingValidator;

    #[async_trait]
    impl Validator for PassingValidator {
        fn kind(&self) -> ValidatorKind {
            ValidatorKind::ReviewAi
        }

        async fn validate(
            &self,
            run: &TaskRun,
            _spec: &TaskSpec,
        ) -> Result<ValidationOutcome, ThalaError> {
            Ok(ValidationOutcome::pass(
                run.run_id.clone(),
                ValidatorKind::ReviewAi,
                "passed",
            ))
        }
    }

    struct UnusedRepo;

    #[async_trait]
    impl RepoProvider for UnusedRepo {
        async fn create_worktree(
            &self,
            _workspace_root: &Path,
            _branch: &str,
            _base_branch: &str,
            _task_id: &str,
        ) -> Result<PathBuf, ThalaError> {
            unreachable!("review failure should not touch repo worktrees")
        }

        async fn remove_worktree(&self, _worktree_path: &Path) -> Result<(), ThalaError> {
            unreachable!("review failure should not touch repo worktrees")
        }

        async fn push_branch(
            &self,
            _workspace_root: &Path,
            _branch: &str,
            _github_token: &str,
        ) -> Result<(), ThalaError> {
            unreachable!("review failure should not push branches")
        }

        async fn get_diff(&self, _worktree_path: &Path) -> Result<String, ThalaError> {
            unreachable!("review failure should not fetch repo diffs")
        }

        async fn create_pr(
            &self,
            _branch: &str,
            _title: &str,
            _body: &str,
        ) -> Result<(u32, String), ThalaError> {
            unreachable!("review failure should not create PRs")
        }

        async fn pr_is_merged(&self, _pr_number: u32) -> Result<bool, ThalaError> {
            unreachable!("review failure should not poll PRs")
        }

        async fn pr_ci_status(&self, _pr_number: u32) -> Result<CiStatus, ThalaError> {
            unreachable!("review failure should not poll CI")
        }

        async fn merge_pr(&self, _pr_number: u32) -> Result<(), ThalaError> {
            unreachable!("review failure should not merge PRs")
        }
    }

    struct PushFailingRepo;

    #[async_trait]
    impl RepoProvider for PushFailingRepo {
        async fn create_worktree(
            &self,
            _workspace_root: &Path,
            _branch: &str,
            _base_branch: &str,
            _task_id: &str,
        ) -> Result<PathBuf, ThalaError> {
            unreachable!("validation should not create worktrees")
        }

        async fn remove_worktree(&self, _worktree_path: &Path) -> Result<(), ThalaError> {
            unreachable!("validation should not remove worktrees")
        }

        async fn push_branch(
            &self,
            _workspace_root: &Path,
            _branch: &str,
            _github_token: &str,
        ) -> Result<(), ThalaError> {
            Err(ThalaError::repo("git push rejected"))
        }

        async fn get_diff(&self, _worktree_path: &Path) -> Result<String, ThalaError> {
            unreachable!("push failure should not fetch repo diffs")
        }

        async fn create_pr(
            &self,
            _branch: &str,
            _title: &str,
            _body: &str,
        ) -> Result<(u32, String), ThalaError> {
            unreachable!("push failure should not create PRs")
        }

        async fn pr_is_merged(&self, _pr_number: u32) -> Result<bool, ThalaError> {
            unreachable!("push failure should not poll PRs")
        }

        async fn pr_ci_status(&self, _pr_number: u32) -> Result<CiStatus, ThalaError> {
            unreachable!("push failure should not poll CI")
        }

        async fn merge_pr(&self, _pr_number: u32) -> Result<(), ThalaError> {
            unreachable!("push failure should not merge PRs")
        }
    }

    struct PrFailingRepo;

    #[async_trait]
    impl RepoProvider for PrFailingRepo {
        async fn create_worktree(
            &self,
            _workspace_root: &Path,
            _branch: &str,
            _base_branch: &str,
            _task_id: &str,
        ) -> Result<PathBuf, ThalaError> {
            unreachable!("validation should not create worktrees")
        }

        async fn remove_worktree(&self, _worktree_path: &Path) -> Result<(), ThalaError> {
            unreachable!("validation should not remove worktrees")
        }

        async fn push_branch(
            &self,
            _workspace_root: &Path,
            _branch: &str,
            _github_token: &str,
        ) -> Result<(), ThalaError> {
            Ok(())
        }

        async fn get_diff(&self, _worktree_path: &Path) -> Result<String, ThalaError> {
            unreachable!("PR creation failure should not fetch repo diffs")
        }

        async fn create_pr(
            &self,
            _branch: &str,
            _title: &str,
            _body: &str,
        ) -> Result<(u32, String), ThalaError> {
            Err(ThalaError::repo("gh pr create failed"))
        }

        async fn pr_is_merged(&self, _pr_number: u32) -> Result<bool, ThalaError> {
            unreachable!("PR creation failure should not poll PRs")
        }

        async fn pr_ci_status(&self, _pr_number: u32) -> Result<CiStatus, ThalaError> {
            unreachable!("PR creation failure should not poll CI")
        }

        async fn merge_pr(&self, _pr_number: u32) -> Result<(), ThalaError> {
            unreachable!("PR creation failure should not merge PRs")
        }
    }

    struct NoopSink;

    #[async_trait]
    impl TaskSink for NoopSink {
        async fn create_task(&self, _spec: NewTaskRequest) -> Result<String, ThalaError> {
            Ok("bd-created".into())
        }

        async fn append_context(&self, _task_id: &str, _context: &str) -> Result<(), ThalaError> {
            Ok(())
        }

        async fn mark_in_progress(&self, _task_id: &str) -> Result<(), ThalaError> {
            Ok(())
        }

        async fn mark_done(&self, _task_id: &str, _pr_number: u32) -> Result<(), ThalaError> {
            Ok(())
        }

        async fn mark_stuck(&self, _task_id: &str, _reason: &str) -> Result<(), ThalaError> {
            Ok(())
        }

        async fn reopen(&self, _task_id: &str) -> Result<(), ThalaError> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct RecordingSink {
        stuck_reasons: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl TaskSink for RecordingSink {
        async fn create_task(&self, _spec: NewTaskRequest) -> Result<String, ThalaError> {
            Ok("bd-created".into())
        }

        async fn append_context(&self, _task_id: &str, _context: &str) -> Result<(), ThalaError> {
            Ok(())
        }

        async fn mark_in_progress(&self, _task_id: &str) -> Result<(), ThalaError> {
            Ok(())
        }

        async fn mark_done(&self, _task_id: &str, _pr_number: u32) -> Result<(), ThalaError> {
            Ok(())
        }

        async fn mark_stuck(&self, _task_id: &str, reason: &str) -> Result<(), ThalaError> {
            self.stuck_reasons.lock().unwrap().push(reason.to_string());
            Ok(())
        }

        async fn reopen(&self, _task_id: &str) -> Result<(), ThalaError> {
            Ok(())
        }
    }

    fn workflow() -> WorkflowConfig {
        serde_yaml::from_str(
            r#"
product: "example"
github_repo: "org/repo"
"#,
        )
        .unwrap()
    }

    fn make_spec(id: &str) -> TaskSpec {
        TaskSpec {
            id: TaskId::new(id),
            title: "Fix review failure path".into(),
            acceptance_criteria: "Review feedback triggers retry".into(),
            context: String::new(),
            beads_ref: id.into(),
            model_override: None,
            always_human_review: false,
            labels: vec![],
        }
    }

    async fn store_completed_run(
        task_id: &TaskId,
        run_id: &RunId,
        attempt: u32,
        worktree_path: Option<String>,
    ) -> (TempDir, Arc<SqliteStateStore>) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteStateStore::open(tmp.path().join("state.db")).unwrap());

        let record = TaskRecord::new(make_spec(task_id.as_str()));
        let record = apply_transition(&record, Transition::MarkReady).unwrap();
        let record = apply_transition(&record, Transition::BeginDispatching).unwrap();
        let mut record = apply_transition(
            &record,
            Transition::RunLaunched {
                run_id: run_id.clone(),
            },
        )
        .unwrap();
        record.attempt = attempt;

        let run = TaskRun::new(
            run_id.clone(),
            task_id.clone(),
            attempt,
            ExecutionBackendKind::Local,
        );
        let run = apply_run_transition(&run, RunTransition::Activated).unwrap();
        let mut run = apply_run_transition(&run, RunTransition::CompletionSignaled).unwrap();
        run.worktree_path = worktree_path;

        store.upsert_task(&record).await.unwrap();
        store.upsert_run(&run).await.unwrap();
        (tmp, store)
    }

    #[tokio::test]
    async fn review_failure_moves_completed_run_to_retry_dispatching() {
        let task_id = TaskId::new("bd-review-retry");
        let run_id = RunId::new_v4();
        let (_tmp, store) = store_completed_run(&task_id, &run_id, 1, None).await;

        let (events_tx, _events_rx) = tokio::sync::mpsc::channel(8);
        let coordinator = ValidatorCoordinator::new(
            workflow(),
            Arc::new(FailingValidator),
            Arc::new(UnusedRepo),
            store.clone(),
            Arc::new(NoopSink),
            vec![],
            events_tx,
        );

        coordinator
            .handle_run_completed(&task_id, &run_id)
            .await
            .unwrap();

        let updated = store.get_task(&task_id).await.unwrap().unwrap();
        assert_eq!(updated.status, TaskStatus::Dispatching);

        let updated_run = store.get_run(&run_id).await.unwrap().unwrap();
        assert_eq!(updated_run.review_cycle, 1);
        assert_eq!(updated_run.review_feedback.as_deref(), Some("needs fix"));
    }

    #[tokio::test]
    async fn review_infrastructure_error_moves_task_to_retry_dispatching() {
        let task_id = TaskId::new("bd-review-infra-retry");
        let run_id = RunId::new_v4();
        let (_tmp, store) = store_completed_run(&task_id, &run_id, 1, None).await;

        let (events_tx, mut events_rx) = tokio::sync::mpsc::channel(8);
        let coordinator = ValidatorCoordinator::new(
            workflow(),
            Arc::new(ErrorValidator),
            Arc::new(UnusedRepo),
            store.clone(),
            Arc::new(NoopSink),
            vec![],
            events_tx,
        );

        coordinator
            .handle_run_completed(&task_id, &run_id)
            .await
            .unwrap();

        let updated = store.get_task(&task_id).await.unwrap().unwrap();
        assert_eq!(updated.status, TaskStatus::Dispatching);

        let updated_run = store.get_run(&run_id).await.unwrap().unwrap();
        assert!(updated_run
            .review_feedback
            .as_deref()
            .unwrap_or_default()
            .contains("review service unavailable"));

        match events_rx.try_recv().unwrap() {
            crate::core::events::OrchestratorEvent::DispatchReady { task_id: id, .. } => {
                assert_eq!(id, task_id);
            }
            event => panic!("expected DispatchReady, got {event:?}"),
        }
    }

    #[tokio::test]
    async fn push_failure_moves_task_to_retry_dispatching() {
        let task_id = TaskId::new("bd-push-infra-retry");
        let run_id = RunId::new_v4();
        let (_tmp, store) =
            store_completed_run(&task_id, &run_id, 1, Some("/tmp/worktree".into())).await;

        let (events_tx, _events_rx) = tokio::sync::mpsc::channel(8);
        let coordinator = ValidatorCoordinator::new(
            workflow(),
            Arc::new(PassingValidator),
            Arc::new(PushFailingRepo),
            store.clone(),
            Arc::new(NoopSink),
            vec![],
            events_tx,
        );

        coordinator
            .handle_run_completed(&task_id, &run_id)
            .await
            .unwrap();

        let updated = store.get_task(&task_id).await.unwrap().unwrap();
        assert_eq!(updated.status, TaskStatus::Dispatching);

        let updated_run = store.get_run(&run_id).await.unwrap().unwrap();
        assert!(updated_run
            .review_feedback
            .as_deref()
            .unwrap_or_default()
            .contains("git push rejected"));
    }

    #[tokio::test]
    async fn pr_creation_failure_moves_task_to_retry_dispatching() {
        let task_id = TaskId::new("bd-pr-infra-retry");
        let run_id = RunId::new_v4();
        let (_tmp, store) =
            store_completed_run(&task_id, &run_id, 1, Some("/tmp/worktree".into())).await;

        let (events_tx, _events_rx) = tokio::sync::mpsc::channel(8);
        let coordinator = ValidatorCoordinator::new(
            workflow(),
            Arc::new(PassingValidator),
            Arc::new(PrFailingRepo),
            store.clone(),
            Arc::new(NoopSink),
            vec![],
            events_tx,
        );

        coordinator
            .handle_run_completed(&task_id, &run_id)
            .await
            .unwrap();

        let updated = store.get_task(&task_id).await.unwrap().unwrap();
        assert_eq!(updated.status, TaskStatus::Dispatching);

        let updated_run = store.get_run(&run_id).await.unwrap().unwrap();
        assert!(updated_run
            .review_feedback
            .as_deref()
            .unwrap_or_default()
            .contains("gh pr create failed"));
    }

    #[tokio::test]
    async fn review_infrastructure_error_at_max_attempts_marks_failed_and_stuck() {
        let task_id = TaskId::new("bd-review-infra-terminal");
        let run_id = RunId::new_v4();
        let (_tmp, store) = store_completed_run(&task_id, &run_id, 3, None).await;
        let sink = Arc::new(RecordingSink::default());

        let (events_tx, mut events_rx) = tokio::sync::mpsc::channel(8);
        let coordinator = ValidatorCoordinator::new(
            workflow(),
            Arc::new(ErrorValidator),
            Arc::new(UnusedRepo),
            store.clone(),
            sink.clone(),
            vec![],
            events_tx,
        );

        coordinator
            .handle_run_completed(&task_id, &run_id)
            .await
            .unwrap();

        let updated = store.get_task(&task_id).await.unwrap().unwrap();
        assert_eq!(updated.status, TaskStatus::Failed);
        assert_eq!(updated.active_run_id, None);
        assert!(events_rx.try_recv().is_err());

        let stuck_reasons = sink.stuck_reasons.lock().unwrap();
        assert_eq!(stuck_reasons.len(), 1);
        assert!(stuck_reasons[0].contains("review service unavailable"));
    }
}

#[cfg(test)]
mod protected_path_tests {
    use super::diff_touches_protected_path;
    use std::fmt::Write;

    fn diff_with_files(files: &[&str]) -> String {
        files.iter().fold(String::new(), |mut diff, f| {
            writeln!(diff, "+++ b/{f}").expect("writing to String cannot fail");
            writeln!(diff, "--- a/{f}").expect("writing to String cannot fail");
            writeln!(diff, "@@ -1 +1 @@").expect("writing to String cannot fail");
            writeln!(diff, "+change").expect("writing to String cannot fail");
            diff
        })
    }

    #[test]
    fn glob_star_star_matches_nested_files() {
        let diff = diff_with_files(&["auth/session.rs", "auth/tokens/jwt.rs"]);
        assert!(diff_touches_protected_path(&diff, &["auth/**".into()]));
    }

    #[test]
    fn double_star_prefix_matches_anywhere() {
        let diff = diff_with_files(&["src/db/migrations/001_init.sql"]);
        assert!(diff_touches_protected_path(
            &diff,
            &["**/migrations/**".into()]
        ));
    }

    #[test]
    fn non_matching_pattern_returns_false() {
        let diff = diff_with_files(&["src/core/task.rs"]);
        assert!(!diff_touches_protected_path(&diff, &["auth/**".into()]));
    }

    #[test]
    fn plain_prefix_without_glob_still_works() {
        let diff = diff_with_files(&["billing/invoices.rs"]);
        assert!(diff_touches_protected_path(&diff, &["billing/**".into()]));
    }

    #[test]
    fn empty_patterns_never_blocks() {
        let diff = diff_with_files(&["anything/file.rs"]);
        assert!(!diff_touches_protected_path(&diff, &[]));
    }
}
