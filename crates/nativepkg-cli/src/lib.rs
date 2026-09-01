//! Building native OS packages from a Node.js project.
//!
//! The work lives here rather than in `main.rs` so integration tests can drive it directly and
//! assert on structures instead of parsed process output.

pub mod cli;
pub mod format;
pub mod introspect;
pub mod report;
pub mod run;
