# Goal Fanout — Review Findings & Proposed Fixes

> Status: discussion draft. Reviews the goal-mode research fanout shipped in
> `477a8a6` (`feat: add goal-mode research fanout with goal coordinator`).
> Audience: colleagues + LLMs reviewing the design. Self-contained.

## Context

Thala is a two-tier orchestration kernel. Tier 1 (this repo) reads canonical
tasks from Beads, launches worker sessions on a configured backend (local,
Modal, Kubernetes, Cloudflare), monitors them, validates results, and escalates
to humans. Tier 2 is the worker sessions, which only see a rendered prompt and
the product codebase.

**Goal mode** is an opt-in path: a Beads task labelled `mode:goal` (with
`goals.enabled = true`) is not sent through the normal code/PR/validation
pipeline. Instead the `GoalCoordinator`
(`src/orchestrator/goal_coordinator.rs`):

1. creates a Thala-owned `GoalRecord`,
2. asks the manager model for a small set of research "variants",
3. launches one child research worker per variant via the existing
   `ExecutionBackend` abstraction,
4. collects a structured JSON report from each worker,
5. synthesizes the reports with the manager model, and
6. decides: append findings + done, ask a human, create follow-up Beads tasks,
   or run another research round.

Each research worker is told to write
`.thala/research/<goal_id>/<run_id>.json` and signal completion. Reports follow
a fixed schema (`ResearchReport` in `src/core/goal.rs`).

The implementation is well-structured and faithful to the design doc
(`docs/contributing/goal-driven-research-fanout.md`): opt-in gating is correct,
normal task flow is untouched, path-traversal guards exist in both the
coordinator and the callback server, callback tokens are constant-time compared,
and there are deterministic fallbacks when no manager API key is present. The
issues below are about the parts that are incomplete or fragile, not the overall
shape.

---

## Issues

### 1 (Critical) — Remote research reports can never be read back

`goal_coordinator.rs` `read_remote_report` does:

```
git fetch origin <branch>
git show origin/<branch>:<artifact>
```

That requires the worker to have **committed and pushed** the report file to its
run branch. But the research prompt (`research_prompt`) tells the worker to write
the JSON file and a signal file, and explicitly says **"Do not create a PR"** —
nothing instructs it to commit or push. The callback payload carries only the
artifact *path*, not its contents.

Result: on any non-local backend (Modal, Cloudflare, Kubernetes),
`load_reports` → `read_remote_report` fails, the error propagates out of
`synthesize_if_ready`, and the goal stalls with no report ever read. The local
backend works because it reads the file directly from the worktree. This
contradicts the acceptance criterion "Child research runs can run on Modal."

> **Verified (review pass).** Confirmed against the code: `plan_and_launch`
> always sets `task_run.remote_branch` for non-local backends
> (`goal_coordinator.rs` ~273–289), so `load_reports` routes every remote run
> through `read_remote_report`. Worth noting the failure is *worse* than "stalls":
> the `synthetic_remote_report` fallback at the bottom of `load_reports` is only
> reached when a run has **neither** a worktree **nor** a remote branch — which
> never happens for remote runs (branch is always `Some`). So remote goals don't
> degrade to a placeholder report; they hard-error out of synthesis. This
> strengthens the case for deleting the git path entirely rather than repairing it.

### 2 (High) — No restart recovery for goals

The acceptance criteria require "restart recovery can reload active goals."
`StateStore::active_goals()` is implemented and unit-tested — but **never called
anywhere**. The `Reconciler` only touches task/run records, not goal records.

A goal interrupted in `Planning`, `Synthesizing`, or `CreatingFollowups` with no
in-flight worker run is stranded forever after a restart. (Goals waiting on a
research run partially recover, because research completion routes through
`research_run_for_worker` — but the goal-level state machine itself is never
re-driven.)

### 3 (High) — Partial launch failure orphans sibling workers

`plan_and_launch` launches runs sequentially in a loop. If run #3's
`backend.launch` fails, `fail_goal_launch` marks the whole goal `Failed` and
returns `Err` — but runs #1 and #2 are already live on the backend and are never
cancelled. They burn resources, call back on completion, and `synthesize_if_ready`
then discards them (goal status is no longer `Researching`). No cleanup path.

Relatedly, `handle_research_run_failed` marks the **entire goal** `Failed` on any
*single* run failure, which is too aggressive — one flaky variant kills an
otherwise-successful fanout.

### 4 (High) — Concurrency limit bypass

The scheduler caps concurrency by counting active task records against
`limits.max_concurrent_runs`. The goal coordinator calls `backend.launch`
directly for up to `max_parallel_research_runs` workers, with **no global slot
accounting**. Multiple concurrent goals (or goals + normal tasks) can blow well
past the configured concurrency ceiling.

### Lower severity (context for the fixes below)

- **#5** `human_question` from manager synthesis is parsed but dropped — human
  tickets use `decision.reason` / a generic string instead.
- **#6** On `ResearchMore`, round-2 runs accumulate alongside round-1 runs;
  synthesis re-reads round-1 artifacts, whose local worktrees may have been
  cleaned up. (Fixed for free by #1's capture-on-completion.)
- No direct `GoalCoordinator` unit tests (only store/config coverage).
- Duplicate variant names from the manager produce colliding `child_task_id`s;
  "distinct approaches" is requested in the prompt but not enforced.

---

## Proposed fixes

Unifying idea: **make `ResearchRun` the single home for a captured report and a
terminal outcome.** That keystone (fix #1) makes #2, #3, and #6 simple.

### Reviewer take — choose the simple contract now

Because this is still rapid experimentation, do **not** preserve two remote
report contracts. Move to inline callback as the only remote contract now, before
external behavior depends on branch artifacts.

The clean version:

- Remote goal workers must deliver the parsed `ResearchReport` in the completion
  callback.
- Local goal workers may still write/read the local report file because local
  monitoring already observes the worktree directly.
- Remove synthesis-time `git fetch` / `git show` report loading for goal mode.
  Remote branch artifacts are a tempting compatibility path, but they keep the
  exact fragile behavior this fix is trying to delete.
- Update the worker wrappers that actually send callbacks (`dev/infra/modal_worker.py`
  and `dev/docker/worker-entrypoint.sh`), not just the LLM-facing prompt. The
  LLM worker can write the JSON file, but the wrapper owns the HTTP callback.

This is a sharper migration, but it is the right tradeoff while the surface area
is small.

### #1 — Capture report at completion, deliver inline for remote

Stop deferring retrieval to synthesis time. Capture the parsed report *once, at
completion*, and store it on the run.

- Add `report: Option<ResearchReport>` to `ResearchRun` (`core/goal.rs`).
- Extend the callback contract to carry the report **inline**:
  `WorkerCallbackPayload { report: Option<ResearchReport>, ... }`.
  `report_artifact` can remain temporarily as local/debug metadata, but it should
  not be the remote retrieval mechanism.
- Update remote worker callback senders:
  - `dev/infra/modal_worker.py`: after the runner succeeds, read the expected
    report JSON from the workspace and include it as `report` in `_send_callback`.
  - `dev/docker/worker-entrypoint.sh`: do the same with `jq`, preserving the
    existing authenticated callback path.
  - Kubernetes/Cloudflare paths should use the same callback payload shape as
    the Docker/Modal wrappers.

> **Make the Cloudflare path concrete, not "same shape" (review pass).** This is
> the keystone fix, and its entire purpose is making *remote* backends work — so
> "K8s/Cloudflare should use the same shape" is the part most likely to bite.
> The Cloudflare worker may not have a Unix-style workspace it can `cat`/`jq` a
> file out of the way the Docker and Modal wrappers do; if it can't read the
> written report JSON and POST it back, the keystone does not actually close the
> acceptance criterion for that backend. Given current work is on
> `feat/cloudflare-slack-backend`, spell out the exact Cloudflare callback site
> (where it reads the report, how it authenticates the callback) in this doc
> before implementation, with the same level of detail as the Modal/Docker
> bullets. K8s can follow the Docker wrapper pattern directly. Treat any backend
> that cannot deliver an inline report as **not goal-capable** rather than
> silently falling back to a broken git read.
- In `handle_research_run_completed`, resolve the report by precedence:
  1. inline report from the callback (required for remote),
  2. worktree file (local backend only).

  Parse + ID-check once, store `research_run.report = Some(report)`.
- `load_reports` collapses to "read `run.report` from each run" — no filesystem
  or git access at synthesis time.

Makes Modal/Cloudflare/Kubernetes work without the worker doing git operations,
and removes the cleanup race in #6. Update `research_prompt` to say "write the
report file; the worker wrapper will include it in the completion callback" so
the LLM is not asked to perform HTTP protocol work it does not own.

If a remote callback succeeds without `report`, treat that research run as
`Failed` with a clear reason. A lost callback already means no completion signal;
keeping git fallback does not actually make the lifecycle reliable enough to
justify the complexity.

### #3 — Tolerate partial failure instead of fail-fast

Dovetails with #1's "terminal outcome on the run."

- In `plan_and_launch`, when `backend.launch` fails for one variant, mark *that*
  `ResearchRun` `Failed` and `continue` the loop — don't `fail_goal_launch` +
  return `Err` (which strands already-launched siblings).
- Change the synthesis gate in `synthesize_if_ready` from "all `Completed`" to
  "**all terminal** (`Completed | Failed`)". Synthesize over the runs that
  produced reports; if *zero* completed, transition the goal to `Failed`.
- Soften `handle_research_run_failed`: mark the single run `Failed` and call
  `synthesize_if_ready`. A goal fails only when *every* variant failed.

No backend `cancel()` API needed — let in-flight siblings finish and weight
synthesis toward the successful ones. More robust than cancellation and survives
restarts.

### #4 — Slot-aware fanout (store as source of truth)

The scheduler caps concurrency by counting active records. Rather than an
in-memory `Semaphore` (doesn't survive restart, needs release plumbing on every
terminal path), keep the store authoritative:

- Before the launch loop in `plan_and_launch`, compute
  `available = max_concurrent_runs.saturating_sub(active_in_flight_count)`.
- If `available == 0`, defer the goal: leave it `Planning`, do not increment
  `research_round`, and launch nothing.
- If `available > 0`, launch `variants.iter().take(available)`.
- Count against the same conceptual pool as the scheduler. The scheduler uses
  active task records, while the coordinator currently has direct access to
  active runs. Either is workable, but the implementation should document the
  chosen count so normal tasks and goal fanout cannot each believe they own the
  same slot.

Consistent with the scheduler's existing "count then take headroom" style; no
new primitive. Same race window the scheduler already tolerates.

### #2 — Goal recovery owned by the coordinator

Don't bloat the `Reconciler`. Give `GoalCoordinator` a `resume_active_goals()`
and call it in `engine.run()` right after `reconciler.reconcile()`:

```rust
for goal in store.active_goals().await? {
    match goal.status {
        Planning          => self.plan_and_launch(goal, spec).await?,        // re-load spec
        Researching       => self.synthesize_if_ready(&goal.goal_id).await?, // no-op if runs still active
        Synthesizing      => self.resume_synthesis(goal).await?,             // re-read stored decision/reports
        WaitingForHuman   => {}                                              // ticket persisted; HumanLoop owns it
        CreatingFollowups => self.resume_followup_creation(goal).await?,
        _terminal         => {}
    }
}
```

Why it's clean with #1 in place:

- Stored reports make `Synthesizing` recovery just "re-read `run.report` and
  finish/retry synthesis" — no worktree dependency.
- In-flight research runs are already `TaskRun`s, so the existing run-recovery
  path completes them; `Researching` recovery only needs the "all already
  finished while we were down" case, which `synthesize_if_ready` covers.

`CreatingFollowups` needs stronger idempotency than "re-run synthesis." The
manager may produce different follow-up tasks on a second call. Store the
accepted `GoalSynthesisDecision` before applying it, then resume from that stored
decision. Also tag created tasks with the goal id (extend
`generated-by:thala-goal` to `generated-by:thala-goal:<goal_id>`) and record or
dedupe created follow-up titles/IDs so a crash during task creation does not
create duplicates.

### #5/#6 — Fold in the small correctness fixes

These are small enough to include with the main coordinator patch:

- Use `decision.human_question` in human tickets when present; fall back to
  `decision.reason`. Note: the deterministic fallback `synthesize_reports` only
  ever emits `CreateTasks` or `Done` — never `AskHuman` — yet it *does* populate
  `human_question` on the `CreateTasks` branch. So the human-ticket plumbing must
  read `human_question` on the `CreateTasks`/follow-up-approval path too, not only
  on `AskHuman`, or the field stays dead in the path that actually sets it.
- Deduplicate/suffix sanitized variant names before constructing `child_task_id`
  so manager duplicate names cannot collide.
- When a second research round is launched, synthesis should only consider the
  current round's terminal runs. Storing reports on `ResearchRun` solves the
  artifact cleanup problem, but the coordinator still needs a clear round filter
  or per-round run association.

---

## Net surface area

- One field on `ResearchRun` (`report`).
- One field on the callback payload (`report`).
- Worker callback wrapper updates so remote backends actually include the report.
- One persisted synthesis decision or equivalent idempotency record for follow-up
  task creation.
- One coordinator method (`resume_active_goals` + a small `resume_synthesis`).
- The synthesis readiness-gate change.
- Deletion of remote git artifact retrieval for goal reports.

Everything else is deletion — the synthesis-time filesystem/git reads largely go
away. **Fix #1 (capture-on-completion) is the keystone**; it makes #2, #3, and #6
fall out.

## Discussion points for reviewers — resolved recommendations

The five open questions from the discussion draft, with a recommended default
for each so this can move from "discuss" to "implement." These are
recommendations, not final decisions — flag any you disagree with on review.

1. **Sole remote report contract?** → **Yes, switch now.** No reason to keep two
   remote contracts while the surface is small; the git path is the exact
   fragility this fix deletes.
2. **Count active records, runs, or both?** → **Match scheduler semantics and
   defer strictly at zero headroom.** The coordinator should count against the
   same conceptual pool the scheduler uses (active task records) so neither path
   believes it owns a slot the other has taken. Document the chosen count inline.
3. **Fail only when every variant fails?** → **Add a configurable
   `min_successful_reports`, default `1`.** "Synthesize over any survivor"
   risks the manager producing confident-looking output from a single
   low-confidence report. Default 1 preserves the doc's "fail only when all fail"
   policy, but make the synthesis prompt aware of *expected vs received* variant
   counts so it can hedge when it sees a degraded fanout. One config field, no
   new machinery.
4. **Idempotency shape for `CreatingFollowups`?** → **Both: store the accepted
   decision *and* dedupe on created tasks.** Tasks are created one at a time in a
   loop with no record of which already succeeded, so a mid-loop crash needs
   resume to query existing tasks by the `generated-by:thala-goal:<goal_id>`
   label and skip ones already made. Stored-decision-only is insufficient on its
   own.
5. **Run resume once or recurring?** → **Recurring.** A goal deferred at zero
   headroom (#4) is never re-driven until the next process restart if resume only
   runs at startup. A recurring sweep gives the deferral a natural retry and
   keeps stranded goals from waiting on an operator restart.

## Test requirements

Add direct `GoalCoordinator`/callback tests before calling this fixed:

- Remote success callback with inline `report` stores `ResearchRun.report` and
  synthesis reads from stored reports only.
- Remote success callback without `report` marks that research run failed with a
  clear reason.
- Local completion can still parse the worktree report file and store it on the
  run.
- One completed run plus one failed run synthesizes over the completed report.
- All failed runs fail the goal.
- Launch failure after earlier successful launches marks only that run failed and
  does not fail the whole goal while siblings are still running.
- `available == 0` defers without incrementing `research_round`.
- Restart resumes `Planning`, `Researching`, `Synthesizing`, and
  `CreatingFollowups` without duplicating follow-up tasks.
- Duplicate manager variant names produce distinct child task IDs.
- Human-action tickets include `human_question` when present — including on the
  `CreateTasks`/follow-up-approval path, not only `AskHuman`.
- `min_successful_reports` policy: with default `1`, one survivor synthesizes;
  with a configured threshold above the surviving count, the goal fails.
- Resume re-drives a goal that was deferred at zero headroom once a slot frees,
  without incrementing `research_round` on the deferral.
- A goal-mode task on the Cloudflare backend delivers an inline `report` in its
  completion callback and never triggers a git fetch/show (guards the keystone on
  the backend it most needs to work on).
