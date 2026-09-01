//! Command line entry point for `nativepkg`.
//!
//! The only crate that uses a type-erased error: library crates return their own enums, and
//! here the only handling is to print the chain and pick an exit status.

use std::io::Write as _;
use std::process::ExitCode;

// Named rather than `as _`: some editors' indexers cannot resolve `Cli::parse` through the
// anonymous import and flag it as missing.
use clap::Parser;

use nativepkg_cli::cli::Cli;
use nativepkg_cli::report::{Reporter, Verbosity};
use nativepkg_cli::{introspect, run};

fn main() -> ExitCode {
    let parsed = Cli::parse();
    let verbosity = if parsed.quiet {
        Verbosity::Quiet
    } else if parsed.verbose {
        Verbosity::Verbose
    } else {
        Verbosity::Normal
    };
    let mut reporter = Reporter::stdio(verbosity);

    if parsed.tool_version {
        println!("nativepkg {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    // A removed option stops the run before anything is written; accepting it silently would
    // let a build script keep working while losing what it asked for.
    let removed = parsed.removed_options();
    if !removed.is_empty() {
        for entry in removed {
            let _ = writeln!(
                reporter.error_stream(),
                "error: {} can no longer be honoured: {}",
                entry.option,
                entry.reason
            );
        }
        return ExitCode::from(2);
    }

    if parsed.is_introspection() {
        return match introspect_and_print(&parsed, &mut reporter) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let _ = writeln!(reporter.error_stream(), "error: {error}");
                ExitCode::FAILURE
            }
        };
    }

    match run::run(&parsed, &mut reporter) {
        Ok(built) => {
            let code = run::summarise(&built, &mut reporter);
            ExitCode::from(u8::try_from(code).unwrap_or(1))
        }
        Err(error) => {
            let _ = writeln!(reporter.error_stream(), "error: {error}");
            for cause in error.chain().skip(1) {
                let _ = writeln!(reporter.error_stream(), "  caused by: {cause}");
            }
            ExitCode::FAILURE
        }
    }
}

fn introspect_and_print(parsed: &Cli, reporter: &mut Reporter) -> anyhow::Result<()> {
    let mut out = std::io::stdout();

    if parsed.list_json_overrides {
        for key in introspect::json_overrides() {
            writeln!(out, "{key}")?;
        }
    }
    if parsed.list_templates {
        for name in introspect::templates() {
            writeln!(out, "{name}")?;
        }
    }
    if parsed.list_template_variables {
        for name in introspect::template_variables() {
            writeln!(out, "{name}")?;
        }
    }
    if parsed.show_readme {
        write!(out, "{}", introspect::readme())?;
    }
    if parsed.show_changelog {
        write!(out, "{}", introspect::changelog())?;
    }
    if let Some(name) = &parsed.cat_template {
        write!(out, "{}", introspect::cat_template(name)?)?;
    }
    let _ = reporter;
    Ok(())
}
