//! Thala orchestration kernel (v1).
//!
//! - [`engine`]      — `OrchestratorEngine` — wires all subsystems
//! - [`scheduler`]   — polls TaskSource, emits DispatchReady events
//! - [`dispatcher`]  — consumes DispatchReady, builds context, launches runs
//! - [`monitor`]     — polls active runs, detects stalls and completions
//! - [`validator`]   — runs review AI + CI validation after run completion
//! - [`human_loop`]  — manages interaction tickets, applies human decisions
//! - [`reconciler`]  — reconciles state after restart

pub mod dispatcher;
pub mod engine;
pub mod human_loop;
pub mod monitor;
pub mod prompt_builder;
pub mod reconciler;
pub mod scheduler;
pub mod validator;
