//! Assembling a [`BuildPlan`] from a resolved configuration and a source tree.
//!
//! Resolution answers "what is this package", collection answers "what does it contain"; this
//! module joins them with one build timestamp. Backends see only the result.

use std::path::{Path, PathBuf};

use crate::core::Result;
use crate::core::collect::collect;
use crate::core::plan::{BuildPlan, Description, Destination, Identity, PlanMetadata, PlannedFile};
use crate::core::resolve::{ResolvedConfig, Warning};
use crate::core::timestamp::{self, Timestamp, TimestampSource};

/// Name this tool reports as the generator of a package.
pub const GENERATOR: &str = "nativepkg";

/// Builds a plan for a project. `inputs` are relative to `project_root`.
pub fn plan(
    config: &ResolvedConfig,
    project_root: &Path,
    inputs: &[PathBuf],
) -> Result<(BuildPlan, Vec<Warning>, TimestampSource)> {
    let (files, warnings) = collect(config, project_root, inputs)?;
    refuse_missing_entrypoints(config, &files)?;

    let sources: Vec<&Path> = files
        .iter()
        .filter_map(|f| match &f.content {
            crate::core::plan::FileContent::FromPath { path, .. } => Some(path.as_path()),
            _ => None,
        })
        .collect();
    let (timestamp, timestamp_source) = timestamp::resolve(project_root, &sources)?;

    let identity = identity_from(config)?;
    let metadata = PlanMetadata {
        generator: GENERATOR.to_owned(),
        generator_version: env!("CARGO_PKG_VERSION").to_owned(),
    };

    let plan = BuildPlan::new(identity, files, timestamp, metadata)?;
    Ok((plan, warnings, timestamp_source))
}

/// A file this tool generates rather than copies, and where it belongs.
///
/// Placement is decided here because it is a fact about Linux, not about a package format.
/// Rendering is not, because the templates carry per-format spellings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceFile {
    pub template: &'static str,
    pub destination: Destination,
    pub mode: u32,
}

const TMPFILES_DIR: &str = "/usr/lib/tmpfiles.d";

/// `/lib/systemd/system` is only a compatibility symlink on merged-`usr` systems; the bash
/// implementation used it.
const SYSTEMD_UNIT_DIR: &str = "/usr/lib/systemd/system";

/// Where `systemctl preset` reads a package's policy for its own units.
///
/// `preset` respects an administrator's decision where `enable` overrides it, but it only
/// applies policy, and Fedora and Arch both ship a final `disable *`. A package that wants its
/// unit enabled ships its own policy here; the administrator's `/etc/systemd/system-preset/`
/// is consulted first and still wins. Found by installing under a booted systemd: on both
/// families the unit came out disabled.
pub const SYSTEMD_PRESET_DIR: &str = "/usr/lib/systemd/system-preset";

/// The preset entry enabling `<package_name>.service`.
///
/// Offered to backends rather than planned for every format: Debian's default policy enables
/// a unit no line matches, so the file changes nothing there and everything on Fedora and
/// Arch. A backend whose default is `disable *` ships it, coupled with its `preset` call.
///
/// `systemd.preset(5)` tells distribution packages to leave policy to the distribution; a
/// package this tool builds is third-party, and openSUSE's guidelines allow exactly that case
/// to install its own preset. The alternatives (`systemctl enable` in `%post`, or leaving
/// the service disabled) do not match what the same project's `.deb` does.
///
/// `50-`: above a distribution's `90-`/`99-` defaults, below an administrator's `00-`.
pub fn systemd_preset_entry(package_name: &str) -> Result<PlannedFile> {
    Ok(PlannedFile::inline(
        Destination::new(format!("{SYSTEMD_PRESET_DIR}/50-{package_name}.preset"))?,
        format!("enable {package_name}.service\n").into_bytes(),
        PlannedFile::MODE_REGULAR,
    ))
}

/// The files this configuration needs generated, in a stable order.
pub fn service_files(config: &ResolvedConfig) -> Result<Vec<ServiceFile>> {
    use crate::core::npm::InitSystem;

    let name = config.package_name.as_str();
    let mut files = Vec::new();

    // Always generated: `collect` plans a `/usr/bin` symlink pointing at it, and omitting it
    // once left every package with a dangling link that no test resolved.
    files.push(ServiceFile {
        template: "executable",
        destination: Destination::new(format!(
            "{}/{name}/bin/{}",
            config.install_dir, config.executable_name
        ))?,
        mode: 0o755,
    });

    if config.init == InitSystem::None {
        return Ok(files);
    }

    files.push(ServiceFile {
        template: "default",
        destination: Destination::new(format!("/etc/default/{name}"))?,
        mode: 0o644,
    });

    let uses = |wanted: InitSystem| config.init == InitSystem::Auto || config.init == wanted;

    if uses(InitSystem::Systemd) {
        files.push(ServiceFile {
            template: "systemd.service",
            destination: Destination::new(format!("{SYSTEMD_UNIT_DIR}/{name}.service"))?,
            mode: 0o644,
        });
        // The log directory as a tmpfiles.d declaration rather than a `mkdir` in a script:
        // recreated at boot if missing. debhelper's `postinst-init-tmpfiles` activates it.
        files.push(ServiceFile {
            template: "tmpfiles.conf",
            destination: Destination::new(format!("{TMPFILES_DIR}/{name}.conf"))?,
            mode: 0o644,
        });
    }
    if uses(InitSystem::Sysv) {
        files.push(ServiceFile {
            template: "sysv-init",
            destination: Destination::new(format!("/etc/init.d/{name}"))?,
            mode: 0o755,
        });
    }
    // Explicit only. Upstart left Debian in stretch and Ubuntu in 15.04; fanning `auto` out
    // to it put a dead `/etc/init/*.conf` in every package, which lintian reports as
    // `package-installs-deprecated-upstart-configuration`.
    if config.init == InitSystem::Upstart {
        files.push(ServiceFile {
            template: "upstart.conf",
            destination: Destination::new(format!("/etc/init/{name}.conf"))?,
            mode: 0o644,
        });
    }

    Ok(files)
}

/// Returns a plan with generated content added to its payload. The caller renders, because it
/// knows the target format's spellings.
pub fn with_generated(
    plan: &BuildPlan,
    generated: Vec<(ServiceFile, String)>,
) -> Result<BuildPlan> {
    let mut files = plan.files.clone();
    for (service, content) in generated {
        let planned = PlannedFile::inline(service.destination, content.into_bytes(), service.mode);

        // A generated file under `/etc` is configuration like any other; without the flag
        // `dpkg` replaces the administrator's edits on every upgrade. `collect` applies this
        // to copied files, and generated ones bypassed it until lintian reported
        // `file-in-etc-not-marked-as-conffile`.
        let planned = if planned.destination.as_str().starts_with("/etc/") {
            planned.as_config()
        } else {
            planned
        };
        files.push(planned);
    }
    BuildPlan::new(
        plan.identity.clone(),
        files,
        plan.timestamp,
        plan.metadata.clone(),
    )
}

/// Every file something in the package executes must be in the package.
///
/// A symlinked entry point whose target resolves outside the tree is dropped by collection
/// under `Tolerate`, and the unit's `ExecStart` or the wrapper then names a path the package
/// lacks: review found `app.js` in neither the plan nor the package. Checked here rather than
/// in collection because the planner's own tests legitimately walk trees with no entry point.
fn refuse_missing_entrypoints(config: &ResolvedConfig, files: &[PlannedFile]) -> Result<()> {
    for (destination, executor) in crate::core::collect::executed_destinations(config) {
        if !files.iter().any(|f| f.destination.as_str() == destination) {
            return Err(crate::core::Error::manifest(format!(
                "`{destination}` is executed by {executor} but is not in the package; the entry \
                 point must be among the inputs and must not resolve outside the project"
            )));
        }
    }
    Ok(())
}

fn identity_from(config: &ResolvedConfig) -> Result<Identity> {
    Ok(Identity {
        package_name: config.package_name.as_str().to_owned(),
        version_deb: config.version.deb().to_owned(),
        version_rpm: config.version.rpm_version().to_owned(),
        release_rpm: config.version.rpm_release().to_owned(),
        epoch: config.version.epoch(),
        description: Description::split(&config.description)?,
        maintainer: config.maintainer.clone(),
        architecture: config.architecture_parsed()?,
        dependencies: config.dependencies.clone(),
        homepage: config.homepage.clone(),
        license: config.license.clone(),
    })
}

/// Builds a plan with an explicit timestamp, for tests and callers that already know it.
///
/// Unlike [`plan`], which the CLI calls, this does not refuse missing entry points: the
/// planner's own tests walk trees with none to check symlinks and modes.
pub fn plan_at(
    config: &ResolvedConfig,
    project_root: &Path,
    inputs: &[PathBuf],
    timestamp: Timestamp,
) -> Result<(BuildPlan, Vec<Warning>)> {
    let (files, warnings) = collect(config, project_root, inputs)?;
    let plan = BuildPlan::new(
        identity_from(config)?,
        files,
        timestamp,
        PlanMetadata {
            generator: GENERATOR.to_owned(),
            generator_version: env!("CARGO_PKG_VERSION").to_owned(),
        },
    )?;
    Ok((plan, warnings))
}

#[cfg(test)]
mod tests {
    use super::{PlannedFile, systemd_preset_entry};
    use crate::core::plan::FileContent;

    #[test]
    fn the_preset_entry_enables_exactly_the_package_unit() {
        let entry = systemd_preset_entry("probe-app").expect("a valid destination");
        assert_eq!(
            entry.destination.as_str(),
            "/usr/lib/systemd/system-preset/50-probe-app.preset"
        );
        assert_eq!(entry.mode, PlannedFile::MODE_REGULAR);
        match &entry.content {
            FileContent::Inline(bytes) => assert_eq!(bytes, b"enable probe-app.service\n"),
            other => panic!("inline content expected, got {other:?}"),
        }
    }
}
