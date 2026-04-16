//! Execution backend adapters.
//!
//! All backends are first-class. No backend is optional.
//!
//! Runtime state that each backend requires persisted in TaskRun:
//!
//! | Backend       | job_handle.job_id                | remote_branch | callback_token |
//! |---------------|----------------------------------|---------------|----------------|
//! | Local         | tmux session name                | None          | None           |
//! | Modal         | Modal app/call ID (ap-xxx)       | Required      | Required       |
//! | Cloudflare    | CF container instance ID         | Required      | Required       |
//! | OpenCodeZen   | Zen session ID (oz-xxx)          | Required      | Required       |

pub mod cloudflare;
pub mod local;
pub mod modal;
pub mod opencode_zen;
pub mod router;

pub use cloudflare::{CloudflareBackend, CloudflareConfig};
pub use local::LocalBackend;
pub use modal::{ModalBackend, ModalConfig};
pub use opencode_zen::{OpenCodeZenBackend, OpenCodeZenConfig};
pub use router::DefaultBackendRouter;
