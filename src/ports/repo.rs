//! RepoProvider port — git and GitHub operations.
//!
//! The orchestrator delegates all repo interactions here so the core
//! orchestration logic has no dependency on git CLI or GitHub API details.
//!
//! Implementation: GitRepoProvider (adapters/repo/git.rs).

use std::path::PathBuf;

use async_trait::async_trait;

use crate::core::error::ThalaError;

// ── CiStatus ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiStatus {
    /// CI is still running.
    Pending,
    /// All required checks passed.
    Passing,
    /// One or more checks failed.
    Failing { failing_checks: Vec<String> },
    /// PR has no CI configured or status is unknown.
    Unknown,
}

// ── RepoProvider ──────────────────────────────────────────────────────────────

/// Git and GitHub operations needed by the orchestrator.
#[async_trait]
pub trait RepoProvider: Send + Sync {
    // ── Worktree management ───────────────────────────────────────────────────

    /// Create a new git worktree for a task run.
    /// Returns the absolute path to the worktree.
    async fn create_worktree(
        &self,
        workspace_root: &std::path::Path,
        branch: &str,
        base_branch: &str,
        task_id: &str,
    ) -> Result<PathBuf, ThalaError>;

    /// Remove a git worktree and its branch.
    async fn remove_worktree(&self, worktree_path: &std::path::Path) -> Result<(), ThalaError>;

    // ── Branch operations ─────────────────────────────────────────────────────

    /// Push a local branch to origin (for remote backends).
    async fn push_branch(
        &self,
        workspace_root: &std::path::Path,
        branch: &str,
        github_token: &str,
    ) -> Result<(), ThalaError>;

    // ── Diff ─────────────────────────────────────────────────────────────────

    /// Return the git diff for a worktree relative to its base branch.
    async fn get_diff(&self, worktree_path: &std::path::Path) -> Result<String, ThalaError>;

    // ── Pull requests ─────────────────────────────────────────────────────────

    /// Create a pull request. Returns (pr_number, pr_url).
    async fn create_pr(
        &self,
        branch: &str,
        title: &str,
        body: &str,
    ) -> Result<(u32, String), ThalaError>;

    /// Check whether a PR has been merged.
    async fn pr_is_merged(&self, pr_number: u32) -> Result<bool, ThalaError>;

    /// Get CI status for a PR.
    async fn pr_ci_status(&self, pr_number: u32) -> Result<CiStatus, ThalaError>;

    /// Merge a PR (squash merge).
    async fn merge_pr(&self, pr_number: u32) -> Result<(), ThalaError>;
}
