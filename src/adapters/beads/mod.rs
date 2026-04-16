//! Beads adapter — the only place Thala reads from and writes to Beads.
//!
//! Boundary rule: nothing outside this module may call `bd` directly.
//! All Beads I/O routes through BeadsTaskSource (reads) or BeadsTaskSink (writes).

pub mod sink;
pub mod source;

pub use sink::BeadsTaskSink;
pub use source::BeadsTaskSource;
