//! Live tests requiring external service credentials.
//!
//! These tests are marked with #[ignore] and only run when explicitly requested
//! with `cargo test --test test_live -- --ignored`.
//!
//! Required environment variables:
//! - ANTHROPIC_API_KEY (for ReviewAiValidator)
//! - MODAL_TOKEN_ID and MODAL_TOKEN_SECRET (for ModalBackend)
//! - CF_API_TOKEN and CF_ACCOUNT_ID (for CloudflareBackend)

mod backend_live;
mod validator_live;
