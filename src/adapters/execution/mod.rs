//! Execution backend adapters.
//!
//! Runtime state that each backend requires persisted in TaskRun:
//!
//! | Backend    | job_handle.job_id            | remote_branch | callback_token |
//! |------------|------------------------------|---------------|----------------|
//! | Local      | tmux session name            | None          | None           |
//! | Modal      | Modal app/call ID (ap-xxx)   | Required      | Required       |
//! | Cloudflare | control-plane remote run ID  | Required      | None           |

pub mod cloudflare;
pub mod local;
pub mod modal;
pub mod router;

pub use cloudflare::{CloudflareBackend, CloudflareConfig};
pub use local::LocalBackend;
pub use modal::{ModalBackend, ModalConfig};
pub use router::DefaultBackendRouter;
