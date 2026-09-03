//! Building native OS packages from any project.
//!
//! The work lives here rather than in `main.rs` so integration tests can drive it directly and
//! assert on structures instead of parsed process output.

pub mod arch;
pub mod cli;
pub mod core;
pub mod deb;
pub mod format;
pub mod introspect;
pub mod report;
pub mod rpm;
pub mod run;
