//! Format-agnostic core of `nativepkg`: manifest resolution, name and version validation,
//! template rendering and file staging, producing a build plan the format backends consume.
//!
//! This crate must never depend on a packaging backend; the dependency arrow points into it
//! from `nativepkg-deb`, `nativepkg-rpm` and `nativepkg-arch`, never out.
//!
//! [`Error`] covers core-stage failures only. Backends define their own error types rather
//! than adding variants here: `#[non_exhaustive]` prevents exhaustive matching, not
//! construction, so a backend could otherwise mint a core error describing an RPM problem.
//! They compose at the CLI boundary through `anyhow`.

pub mod arch;
pub mod build;
pub mod collect;
pub mod error;
pub mod name;
pub mod npm;
pub mod plan;
pub mod resolve;
pub mod scratch;
pub mod template;
pub mod text;
pub mod timestamp;
pub mod version;

pub use arch::Architecture;
pub use error::{Error, Result};
pub use name::{PackageName, UnixName};
pub use npm::Manifest;
pub use plan::{BuildPlan, Description, Destination, EntryKind, FileContent, PlannedFile};
pub use resolve::{Overrides, ResolvedConfig, Warning, resolve};
pub use timestamp::{Timestamp, TimestampSource};
pub use version::{MappedVersion, VersionSpec};
