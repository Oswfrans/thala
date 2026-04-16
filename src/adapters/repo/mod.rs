//! GitRepoProvider — git worktree and GitHub PR operations.
//!
//! All git and GitHub I/O lives here. The orchestrator calls this only via
//! the RepoProvider trait, never touching git or GitHub directly.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::core::error::ThalaError;
use crate::ports::repo::{CiStatus, RepoProvider};

// ── GitRepoProvider ───────────────────────────────────────────────────────────

pub struct GitRepoProvider {
    /// GitHub repository slug, e.g. "org/repo".
    pub github_repo: String,

    /// Environment variable name that holds the GitHub token.
    pub github_token_env: String,
}

impl GitRepoProvider {
    pub fn new(github_repo: impl Into<String>, github_token_env: impl Into<String>) -> Self {
        Self {
            github_repo: github_repo.into(),
            github_token_env: github_token_env.into(),
        }
    }

    fn github_token(&self) -> Result<String, ThalaError> {
        std::env::var(&self.github_token_env).map_err(|_| {
            ThalaError::Repo(format!(
                "Environment variable '{}' is not set",
                self.github_token_env
            ))
        })
    }

    async fn run_git(&self, args: &[&str], cwd: &Path) -> Result<String, ThalaError> {
        let output = tokio::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .await
            .map_err(|e| ThalaError::Repo(format!("git spawn failed: {e}")))?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(ThalaError::Repo(format!(
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            )))
        }
    }

    async fn run_gh(&self, args: &[&str]) -> Result<String, ThalaError> {
        let output = tokio::process::Command::new("gh")
            .args(args)
            .output()
            .await
            .map_err(|e| ThalaError::Repo(format!("gh spawn failed: {e}")))?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(ThalaError::Repo(format!(
                "gh {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            )))
        }
    }
}

// ── RepoProvider impl ─────────────────────────────────────────────────────────

#[async_trait]
impl RepoProvider for GitRepoProvider {
    async fn create_worktree(
        &self,
        workspace_root: &Path,
        branch: &str,
        base_branch: &str,
        task_id: &str,
    ) -> Result<PathBuf, ThalaError> {
        let slug = task_id.replace(['/', ':'], "-");
        let worktree_path = workspace_root.join(format!(".thala-worktrees/{slug}"));

        // Create branch from base and add worktree.
        self.run_git(
            &[
                "worktree",
                "add",
                "-b",
                branch,
                worktree_path.to_str().unwrap(),
                base_branch,
            ],
            workspace_root,
        )
        .await?;

        Ok(worktree_path)
    }

    async fn remove_worktree(&self, worktree_path: &Path) -> Result<(), ThalaError> {
        // Determine workspace root (parent of .thala-worktrees/).
        let workspace_root = worktree_path
            .parent()
            .and_then(|p| p.parent())
            .ok_or_else(|| {
                ThalaError::Repo("Cannot determine workspace root from worktree path".into())
            })?;

        self.run_git(
            &[
                "worktree",
                "remove",
                "--force",
                worktree_path.to_str().unwrap(),
            ],
            workspace_root,
        )
        .await?;
        Ok(())
    }

    async fn push_branch(
        &self,
        workspace_root: &Path,
        branch: &str,
        _github_token: &str,
    ) -> Result<(), ThalaError> {
        self.run_git(
            &["push", "--set-upstream", "origin", branch],
            workspace_root,
        )
        .await?;
        Ok(())
    }

    async fn get_diff(&self, worktree_path: &Path) -> Result<String, ThalaError> {
        self.run_git(&["diff", "HEAD"], worktree_path).await
    }

    async fn create_pr(
        &self,
        branch: &str,
        title: &str,
        body: &str,
    ) -> Result<(u32, String), ThalaError> {
        let output = self
            .run_gh(&[
                "pr",
                "create",
                "--repo",
                &self.github_repo,
                "--head",
                branch,
                "--title",
                title,
                "--body",
                body,
            ])
            .await?;

        // `gh pr create` outputs the PR URL on stdout.
        let url = output.trim().to_owned();
        // Extract PR number from URL: last path segment.
        let number: u32 = url
            .rsplit('/')
            .next()
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| ThalaError::Repo(format!("Cannot parse PR number from URL: {url}")))?;
        Ok((number, url))
    }

    async fn pr_is_merged(&self, pr_number: u32) -> Result<bool, ThalaError> {
        let result = self
            .run_gh(&[
                "pr",
                "view",
                &pr_number.to_string(),
                "--repo",
                &self.github_repo,
                "--json",
                "state",
                "--jq",
                ".state",
            ])
            .await?;
        Ok(result.trim() == "MERGED")
    }

    async fn pr_ci_status(&self, pr_number: u32) -> Result<CiStatus, ThalaError> {
        let output = self
            .run_gh(&[
                "pr",
                "checks",
                &pr_number.to_string(),
                "--repo",
                &self.github_repo,
                "--json",
                "name,state",
            ])
            .await?;

        let checks: Vec<serde_json::Value> =
            serde_json::from_str(&output).map_err(|e| ThalaError::Repo(e.to_string()))?;

        if checks.is_empty() {
            return Ok(CiStatus::Unknown);
        }

        let mut failing = Vec::new();
        let mut any_pending = false;

        for check in &checks {
            let state = check["state"].as_str().unwrap_or("UNKNOWN");
            let name = check["name"].as_str().unwrap_or("unknown").to_owned();
            match state {
                "IN_PROGRESS" | "QUEUED" | "WAITING" => any_pending = true,
                "FAILURE" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED" => {
                    failing.push(name);
                }
                _ => {}
            }
        }

        if !failing.is_empty() {
            return Ok(CiStatus::Failing {
                failing_checks: failing,
            });
        }
        if any_pending {
            return Ok(CiStatus::Pending);
        }
        Ok(CiStatus::Passing)
    }

    async fn merge_pr(&self, pr_number: u32) -> Result<(), ThalaError> {
        let token = self.github_token()?;
        // Use gh with squash merge.
        let _ = token; // Token is in env, gh CLI reads it automatically.
        self.run_gh(&[
            "pr",
            "merge",
            &pr_number.to_string(),
            "--repo",
            &self.github_repo,
            "--squash",
            "--delete-branch",
        ])
        .await?;
        Ok(())
    }
}
