# Goal: Add Goal-Driven Research Fanout While Preserving Normal Coding Tasks

## Objective

Teach Thala to handle a higher-level "goal" task by planning and launching multiple research workers, collecting structured reports, synthesizing the results with the manager model, and deciding the next action.

Thala must continue to support ordinary coding tasks exactly as it does today. Autoresearch and goal capabilities must be opt-in per task, workflow, or run, not default behavior.

## Background

Today Thala is task/run oriented:

- Beads provides canonical task truth.
- Thala dispatches one worker run per task attempt.
- Worker completion flows into validation, PR creation, CI, merge, or human approval.
- Pi currently defaults to installing `autoresearch` and `goal`, but that is incorrect. Those extensions should be invoked only when a task or goal explicitly requests them.

We want a new orchestration mode:

> A human gives Thala a goal. Thala plans several research approaches, bursts workers on Modal, Kubernetes, or local backends, gathers reports, synthesizes findings, and either proposes an action, asks a human, or creates follow-up implementation tasks.

## Non-Goals

- Do not replace normal Beads coding task execution.
- Do not make every worker an autoresearch or goal worker.
- Do not introduce a general-purpose multi-agent framework.
- Do not let workers see Thala internal orchestration state.
- Do not auto-merge `thala-core`.
- Do not make Pi extensions part of core orchestration correctness.
- Do not require Modal; the design should work through the existing `ExecutionBackend` abstraction.

## Required Behavior

### 1. Normal Task Mode Remains Default

A normal Beads task without explicit research or goal metadata should behave as today:

- Scheduler sees ready task.
- Dispatcher launches one worker run.
- Worker receives the normal rendered prompt.
- Completion enters validation, PR, and CI flow.
- No autoresearch or goal extensions are installed unless explicitly configured.

### 2. Fix Worker Extension Defaults

`autoresearch` and `goal` must not be default Pi extensions.

Default worker config should be minimal:

```yaml
worker:
  runner: pi
  extensions: []
```

or equivalent.

A workflow may opt in:

```yaml
worker:
  runner: pi
  extensions:
    - autoresearch
    - goal
```

Task-level opt-in should also be possible through labels or metadata, for example:

```text
labels:
  - mode:goal
  - worker-extension:autoresearch
  - worker-extension:goal
```

The exact metadata shape can be chosen during implementation, but it must be explicit.

### 3. Add Goal Mode

A Beads task can be interpreted as a goal when explicitly marked, for example:

```text
labels:
  - mode:goal
```

Goal tasks should not immediately enter the normal code validation path.

Instead, Thala should:

1. Create a Thala-owned `GoalRecord`.
2. Ask the manager model to produce a small research plan.
3. Launch multiple child research runs.
4. Collect structured reports.
5. Synthesize the reports.
6. Decide one of:
   - append findings to Beads and mark done,
   - ask a human for a decision,
   - create follow-up Beads coding tasks,
   - launch another research round within configured limits.

### 4. Add Runtime Goal State

Add Thala-owned runtime state separate from Beads task truth.

Suggested types:

```rust
pub struct GoalRecord {
    pub goal_id: GoalId,
    pub source_task_id: TaskId,
    pub title: String,
    pub objective: String,
    pub status: GoalStatus,
    pub research_round: u32,
    pub max_rounds: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub enum GoalStatus {
    Planning,
    Researching,
    Synthesizing,
    WaitingForHuman,
    CreatingFollowups,
    Succeeded,
    Failed,
    Resolved,
}
```

Suggested child run type:

```rust
pub struct ResearchRun {
    pub research_run_id: RunId,
    pub goal_id: GoalId,
    pub task_id: TaskId,
    pub variant_name: String,
    pub prompt: String,
    pub backend: ExecutionBackendKind,
    pub worker_run_id: RunId,
    pub status: ResearchRunStatus,
    pub report_artifact: Option<String>,
}
```

Implementation may reuse `TaskRun` if clean, but goal and child relationships must be explicit.

### 5. Structured Research Output Contract

Each research worker must produce a structured artifact, not just logs.

Suggested path:

```text
.thala/research/<goal_id>/<run_id>.json
```

Suggested schema:

```json
{
  "goal_id": "string",
  "run_id": "string",
  "variant": "string",
  "summary": "string",
  "findings": [
    {
      "claim": "string",
      "evidence": "string",
      "source": "string|null",
      "confidence": "low|medium|high"
    }
  ],
  "recommendation": "string",
  "risks": ["string"],
  "follow_up_questions": ["string"],
  "suggested_tasks": [
    {
      "title": "string",
      "acceptance_criteria": "string",
      "context": "string"
    }
  ]
}
```

Callbacks may remain small, but Thala must be able to locate and read the report artifact.

### 6. Add Goal Coordinator

Add a new orchestrator subsystem beside the existing scheduler, dispatcher, monitor, and validator stack.

Suggested module:

```text
src/orchestrator/goal_coordinator.rs
```

Responsibilities:

- detect explicit goal-mode tasks,
- create `GoalRecord`,
- generate research variants,
- launch child runs through existing backend abstractions,
- wait for child run completion,
- read reports,
- synthesize findings,
- update Beads or request human input.

Do not put this logic inside the normal validator.

### 7. Add Events

Suggested new events:

```rust
GoalReady { goal_id, task_id, at }
GoalPlanned { goal_id, variants, at }
ResearchRunLaunched { goal_id, run_id, at }
ResearchRunCompleted { goal_id, run_id, report_artifact, at }
GoalSynthesisReady { goal_id, at }
GoalActionRequired { goal_id, reason, at }
GoalCompleted { goal_id, at }
GoalFailed { goal_id, reason, at }
```

Events should carry IDs and minimal facts. Durable truth remains in `StateStore`.

### 8. Manager Synthesis

After all required child research runs complete, Thala should call the manager model with:

- original goal,
- acceptance criteria,
- all structured research reports,
- constraints from `WORKFLOW.md`,
- available actions.

The manager output must be structured JSON.

Suggested actions:

```json
{
  "decision": "done|ask_human|create_tasks|research_more|failed",
  "summary": "string",
  "human_question": "string|null",
  "follow_up_tasks": [],
  "reason": "string"
}
```

### 9. Human Interaction

If synthesis cannot confidently choose an action, Thala should route through the existing `InteractionLayer`.

Human actions should include:

- approve follow-up tasks,
- request another research round,
- resolve goal,
- cancel goal,
- reroute backend,
- convert to normal implementation task.

### 10. Configuration

Add explicit workflow config for goal mode.

Example:

```yaml
goals:
  enabled: true
  default_backend: modal
  max_parallel_research_runs: 5
  max_research_rounds: 2
  require_human_before_followup_tasks: true
  worker_extensions:
    - autoresearch
    - goal
```

This must not change default behavior for normal coding tasks.

If `goals.enabled` is false or absent, `mode:goal` tasks should fail clearly or be skipped with a warning.

## Acceptance Criteria

- Normal Beads coding tasks still run without autoresearch or goal extensions by default.
- Pi default extensions are empty or minimal; `autoresearch` and `goal` are explicit opt-ins.
- A task marked `mode:goal` creates a durable `GoalRecord`.
- Goal mode launches multiple child research runs through existing `ExecutionBackend`.
- Child research runs can run on Modal without changing the `ExecutionBackend` trait in a backend-specific way.
- Each research child produces a structured JSON report artifact.
- Thala stores enough state to recover goal progress after restart.
- Thala synthesizes completed research reports with the manager model.
- Synthesis can append results to Beads, ask a human, or create follow-up Beads tasks.
- Goal-mode runs do not enter the normal PR validation pipeline unless they create explicit implementation tasks.
- Human interaction works for ambiguous or high-risk synthesis decisions.
- Existing normal validator, PR, and CI flow remains unchanged for non-goal tasks.
- Tests cover:
  - default worker extensions do not include `autoresearch` or `goal`,
  - explicit workflow extensions are passed to workers,
  - normal task dispatch still works,
  - goal-mode task creates a goal record,
  - goal coordinator launches N child runs,
  - research callback/report completion updates goal state,
  - synthesis creates the expected action,
  - restart recovery can reload active goals,
  - malformed research reports fail clearly,
  - goal mode disabled produces a clear error.

## Suggested Implementation Order

1. Fix Pi extension defaults.
2. Add explicit worker extension opt-in plumbing if task-level override is missing.
3. Add `GoalRecord` and persistence.
4. Add research report artifact schema and parser.
5. Add goal coordinator with mocked backend tests.
6. Add manager synthesis with mocked model tests.
7. Wire goal-mode routing from Beads labels.
8. Add Modal/Kubernetes smoke coverage for research child runs.
9. Update docs and examples.

## Risk

High risk.

Touches orchestration state, worker launch semantics, callback/report contracts, and task routing. Keep changes staged. The first PR should only fix default extensions and preserve normal task behavior.
