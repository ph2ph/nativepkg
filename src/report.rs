//! What the user sees, and where it goes.
//!
//! Diagnostics go to stderr and results to stdout, so `--json` stays parseable when there are
//! warnings. The bash implementation mixed the two.

use std::io::{self, Write};

use crate::core::plan::{BuildPlan, EntryKind};
use crate::core::resolve::ResolvedConfig;
use anyhow::Result;

use crate::format::Format;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
    Quiet,
    Normal,
    Verbose,
}

pub struct Reporter {
    verbosity: Verbosity,
    out: Box<dyn Write>,
    err: Box<dyn Write>,
}

impl Reporter {
    #[must_use]
    pub fn stdio(verbosity: Verbosity) -> Self {
        Self {
            verbosity,
            out: Box::new(io::stdout()),
            err: Box::new(io::stderr()),
        }
    }

    /// For tests: write wherever the caller says.
    #[must_use]
    pub fn new(verbosity: Verbosity, out: Box<dyn Write>, err: Box<dyn Write>) -> Self {
        Self {
            verbosity,
            out,
            err,
        }
    }

    pub fn error_stream(&mut self) -> &mut dyn Write {
        &mut self.err
    }

    pub fn warn(&mut self, message: &str) {
        if self.verbosity != Verbosity::Quiet {
            let _ = writeln!(self.err, "warning: {message}");
        }
    }

    pub fn detail(&mut self, message: &str) {
        if self.verbosity == Verbosity::Verbose {
            let _ = writeln!(self.err, "note: {message}");
        }
    }

    pub fn produced(&mut self, format: Format, path: &std::path::Path) {
        if self.verbosity != Verbosity::Quiet {
            let _ = writeln!(self.out, "{}", path.display());
        }
        let _ = format;
    }

    /// Always shown, with the whole cause chain: the outermost message says what was being
    /// attempted, the innermost what went wrong, and either alone is rarely enough.
    pub fn failed(&mut self, format: Format, error: &anyhow::Error) {
        let _ = writeln!(self.err, "error: {format}: {error}");
        for cause in error.chain().skip(1) {
            let _ = writeln!(self.err, "  caused by: {cause}");
        }
    }

    /// Describes what a build would do, without doing it.
    pub fn dry_run(
        &mut self,
        plan: &BuildPlan,
        config: &ResolvedConfig,
        formats: &[Format],
        json: bool,
    ) -> Result<()> {
        if json {
            let names: Vec<String> = formats.iter().map(|f| f.name().to_owned()).collect();
            let files: Vec<serde_json::Value> = plan
                .files
                .iter()
                .map(|file| {
                    serde_json::json!({
                        "destination": file.destination.to_string(),
                        "kind": match file.kind {
                            EntryKind::Regular => "file",
                            EntryKind::Directory => "directory",
                            EntryKind::Symlink { .. } => "symlink",
                        },
                        "mode": format!("{:o}", file.mode),
                    })
                })
                .collect();

            let document = serde_json::json!({
                "package": plan.identity.package_name,
                "formats": names,
                "versions": formats.iter().map(|f| {
                    serde_json::json!({ "format": f.name(), "version": f.version_of(config) })
                }).collect::<Vec<_>>(),
                "installed_size_kib": plan.installed_size_kib(),
                "timestamp": plan.timestamp.as_secs(),
                "files": files,
            });
            writeln!(self.out, "{}", serde_json::to_string_pretty(&document)?)?;
        } else {
            writeln!(
                self.out,
                "{} {} would be built for: {}",
                plan.identity.package_name,
                config.version.deb(),
                formats
                    .iter()
                    .map(|f| f.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            )?;
            for file in &plan.files {
                writeln!(self.out, "  {}", file.destination)?;
            }
        }
        Ok(())
    }
}
