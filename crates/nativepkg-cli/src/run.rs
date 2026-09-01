//! Resolving, planning and building, in that order.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use nativepkg_core::npm::Settings;
use nativepkg_core::plan::BuildPlan;
use nativepkg_core::resolve::{Overrides, ResolvedConfig, Warning};
use nativepkg_core::{Manifest, build, resolve};

use crate::cli::Cli;
use crate::format::Format;
use crate::report::Reporter;

/// What one format's build produced.
pub struct Built {
    pub format: Format,
    pub outcome: Result<PathBuf>,
}

/// Runs a build for every selected format, returning per-format outcomes rather than the
/// first failure: "two succeeded, one failed and why" beats a stop at the first problem.
///
/// # Errors
///
/// Only when nothing could be attempted: the manifest, the resolution or the plan failed.
pub fn run(cli: &Cli, reporter: &mut Reporter) -> Result<Vec<Built>> {
    let project_root = std::env::current_dir().context("reading the working directory")?;
    let manifest = load_manifest(&project_root)?;

    let overrides = overrides_from(cli, reporter);
    let (config, warnings) = resolve(&manifest, &overrides).context("resolving configuration")?;
    report_warnings(reporter, &warnings);

    let inputs = inputs_for(cli, &config, &project_root)?;
    let (plan, plan_warnings, timestamp_source) =
        build::plan(&config, &project_root, &inputs).context("planning the package")?;
    report_warnings(reporter, &plan_warnings);
    reporter.detail(&format!("timestamp source: {timestamp_source:?}"));

    let formats = selected_formats(cli);
    for format in &formats {
        if let Some(reason) = format.unsupported(config.init) {
            reporter.warn(&format!("{format}: {reason}"));
        }
    }

    let overrides = cli.template_overrides();

    if cli.dry_run {
        // Generated files are added here too, or the dry run describes a package missing the
        // unit, the wrapper and the defaults file.
        let variables = formats
            .first()
            .copied()
            .unwrap_or(Format::Deb)
            .variables(&config, env!("CARGO_PKG_VERSION"))?;
        let described = generate(&plan, &config, &variables, &overrides)?;
        reporter.dry_run(&described, &config, &formats, cli.json)?;
        return Ok(Vec::new());
    }

    Ok(build_each(
        &formats,
        &plan,
        &config,
        &cli.output_dir,
        &overrides,
        reporter,
    ))
}

fn build_each(
    formats: &[Format],
    plan: &BuildPlan,
    config: &ResolvedConfig,
    output_dir: &Path,
    overrides: &[(&'static str, &Path)],
    reporter: &mut Reporter,
) -> Vec<Built> {
    let generator_version = env!("CARGO_PKG_VERSION");
    let mut built = Vec::with_capacity(formats.len());

    for format in formats {
        let outcome = format
            .variables(config, generator_version)
            .and_then(|variables| {
                // Rendered per format: the vocabulary carries per-format spellings. The shared
                // plan is never modified; each format derives its own.
                let derived = generate(plan, config, &variables, overrides)?;
                let path = format.build(&derived, config, &variables, output_dir)?;
                Ok(path)
            });

        match &outcome {
            Ok(path) => reporter.produced(*format, path),
            Err(error) => reporter.failed(*format, error),
        }
        built.push(Built {
            format: *format,
            outcome,
        });
    }
    built
}

/// Renders the generated service files into a plan. `collect` plans a `/usr/bin` symlink at
/// the wrapper, so without this the package shipped a dangling link.
fn generate(
    plan: &BuildPlan,
    config: &ResolvedConfig,
    variables: &nativepkg_core::template::Variables,
    overrides: &[(&'static str, &Path)],
) -> Result<BuildPlan> {
    use nativepkg_core::template;

    let mut rendered = Vec::new();

    for service in build::service_files(config).context("placing the generated files")? {
        let override_path = overrides
            .iter()
            .find(|(name, _)| *name == service.template)
            .map(|(_, path)| *path);
        let source = template::load(service.template, override_path)
            .with_context(|| format!("loading template `{}`", service.template))?;
        let text = template::render(service.template, &source, variables)
            .with_context(|| format!("rendering template `{}`", service.template))?;
        rendered.push((service, text));
    }

    build::with_generated(plan, rendered).context("adding the generated files to the plan")
}

fn selected_formats(cli: &Cli) -> Vec<Format> {
    let mut formats = cli.format.clone();
    formats.sort_unstable();
    formats.dedup();
    formats
}

/// The inputs to package, defaulting to the whole project when none were named.
fn inputs_for(cli: &Cli, config: &ResolvedConfig, project_root: &Path) -> Result<Vec<PathBuf>> {
    if !cli.inputs.is_empty() {
        return Ok(cli.inputs.clone());
    }

    // The bash tool required inputs after `--`; defaulting to the manifest and the application
    // tree is what all its fixtures passed anyway. Entry points come from the resolver, not a
    // guessed list: a fixed list once omitted `app.js` and packaged everything but the file the
    // unit runs.
    let mut inputs = Vec::new();
    for entry in [&config.daemon_entrypoint, &config.cli_entrypoint]
        .into_iter()
        .flatten()
    {
        let path = PathBuf::from(entry.as_str());
        if project_root.join(&path).exists() && !inputs.contains(&path) {
            inputs.push(path);
        }
    }
    for candidate in ["package.json", "lib", "src", "bin", "index.js"] {
        let path = PathBuf::from(candidate);
        if project_root.join(&path).exists() && !inputs.contains(&path) {
            inputs.push(path);
        }
    }
    if inputs.is_empty() {
        bail!("nothing to package: name files after the options, or add a `lib`/`src` directory");
    }
    Ok(inputs)
}

/// The project's settings, from a `.nativepkg` file when present; otherwise everything is
/// supplied on the command line.
///
/// `package.json` is deliberately never read — neither its top-level fields nor any `nativepkg`
/// object in it. Configuration lives in exactly one place, so there is nothing to keep in sync
/// and no divergence between two spellings of the same key.
fn load_manifest(project_root: &Path) -> Result<Manifest> {
    let mut manifest = Manifest::default();
    let nativepkg_path = project_root.join(".nativepkg");
    if nativepkg_path.exists() {
        let settings = Settings::from_path(&nativepkg_path)
            .with_context(|| format!("reading {}", nativepkg_path.display()))?;
        manifest.nativepkg = Some(settings);
    }
    Ok(manifest)
}

fn overrides_from(cli: &Cli, reporter: &mut Reporter) -> Overrides {
    let architecture = match (&cli.architecture, &cli.arch_deprecated) {
        (Some(current), _) => Some(current.clone()),
        (None, Some(legacy)) => {
            reporter.warn("`--arch` is deprecated; use `--architecture`");
            Some(legacy.clone())
        }
        (None, None) => None,
    };

    Overrides {
        package_name: cli.package_name.clone(),
        version: cli.version.clone(),
        epoch: cli.epoch,
        description: cli.description.clone(),
        maintainer: cli.maintainer.clone(),
        architecture,
        dependencies: cli.deps.clone(),
        install_dir: cli.install_dir.clone(),
        user: cli.user.clone(),
        group: cli.group.clone(),
        executable_name: cli.executable_name.clone(),
        output_deb_name: cli.output_name.clone(),
        extra_files: cli.extra_files.clone(),
        triggers_file: cli.triggers_file.clone(),
        daemon_entrypoint: cli.daemon_entrypoint.clone(),
        cli_entrypoint: cli.cli_entrypoint.clone(),
        // Validated by the parser: an unknown value is refused with the accepted spellings
        // listed, so nothing is dropped here.
        init: cli.init.map(Into::into),
        install_strategy: cli.install_strategy.map(Into::into),
        install_command: cli.install_command.clone(),
        install_binary: cli.install_binary.clone(),
        include_nodejs: cli.nodejs,
    }
}

/// Warnings go out before any package is written: printed after, they read as a remark on
/// something already done rather than a reason to stop.
fn report_warnings(reporter: &mut Reporter, warnings: &[Warning]) {
    for warning in warnings {
        reporter.warn(&warning.to_string());
    }
}

/// Writes the final summary and returns the process exit code.
#[must_use]
pub fn summarise(built: &[Built], reporter: &mut Reporter) -> i32 {
    let failed = built.iter().filter(|b| b.outcome.is_err()).count();
    if failed == 0 {
        return 0;
    }

    let _ = writeln!(
        reporter.error_stream(),
        "{failed} of {} formats failed",
        built.len()
    );
    1
}
