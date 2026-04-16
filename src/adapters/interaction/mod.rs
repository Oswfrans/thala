//! Interaction adapters — human approval and escalation channels.
//!
//! These are SEPARATE from intake adapters. Interaction handles approvals,
//! retries, and escalations on tasks that are already in Thala's pipeline.

pub mod discord;
pub mod slack;
