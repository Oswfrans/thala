//! Thala — an opinionated orchestration kernel for managed coding tasks.
//!
//! # Architecture (ports/adapters)
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │  core         — pure domain types, state machine, events │
//! │  ports        — async trait definitions (no I/O here)    │
//! │  adapters     — all I/O: Beads, Slack, Discord, backends │
//! │  orchestrator — engine, scheduler, dispatcher, monitor   │
//! └─────────────────────────────────────────────────────────┘
//! ```

#![warn(clippy::all, clippy::pedantic)]
#![allow(
    clippy::doc_markdown,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::module_name_repetitions,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::uninlined_format_args,
    dead_code
)]

pub mod adapters;
pub mod core;
pub mod orchestrator;
pub mod ports;
