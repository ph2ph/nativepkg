//! RPM (`.rpm`) backend for `nativepkg`.
//!
//! Consumes the format-agnostic build plan from [`nativepkg_core`] and writes an `.rpm` — lead,
//! signature, header, compressed cpio payload — in-process through the `rpm` crate, so an RPM
//! can be built on a Debian host: `rpmbuild` is not installable on this project's CI runner.

pub mod error;
pub mod scriptlets;

use std::path::{Path, PathBuf};

use nativepkg_core::plan::{BuildPlan, EntryKind, FileContent, PlannedFile};
use nativepkg_core::scratch::OutputFile;
use rpm::{BuildConfig, Dependency, FileMode, FileOptions, PackageBuilder};

pub use error::{Error, Result};

/// RPM requires a licence; saying the manifest declared none is honest, inventing one is not.
const UNKNOWN_LICENSE: &str = "UNKNOWN";

/// How to write a package.
///
/// Not `#[non_exhaustive]`: other crates build this with struct expressions, which that blocks.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Scriptlets as `(name, contents)`, named in RPM's own vocabulary — `pre`, `post`, `preun`,
    /// `postun` — never dpkg's: dpkg's `$1` is an action word, RPM's is a package count.
    pub maintainer_scripts: Vec<(String, Vec<u8>)>,
    /// Service whose unit the scriptlets `preset`, so the package must also ship the preset
    /// policy ([`nativepkg_core::build::systemd_preset_entry`]); Fedora's `disable *` default
    /// otherwise leaves the service disabled. Decided with [`scriptlets::uses_preset`].
    pub preset_service: Option<String>,
}

/// Writes an `.rpm` for `plan` into `output_dir`, returning the path written.
///
/// # Errors
///
/// [`Error::Io`] for the output or a source, [`Error::Unrepresentable`] for a plan entry with
/// no RPM equivalent, [`Error::Rpm`] when the crate rejects the package.
pub fn build(plan: &BuildPlan, options: &Options, output_dir: &Path) -> Result<PathBuf> {
    let package = assemble(plan, options)?;
    std::fs::create_dir_all(output_dir).map_err(|e| Error::io(output_dir.to_path_buf(), e))?;
    let mut output = OutputFile::create(output_dir.join(file_name(plan)))?;
    package
        .write(&mut output)
        .map_err(|e| Error::rpm("could not serialise the package", e))?;
    Ok(output.finish()?)
}

/// Assembles an `.rpm` in memory.
fn assemble(plan: &BuildPlan, options: &Options) -> Result<rpm::Package> {
    let with_policy = with_preset(plan, options)?;
    let plan = &with_policy;
    let identity = &plan.identity;

    let mut builder = PackageBuilder::new(
        &identity.package_name,
        &identity.version_rpm,
        identity.license.as_deref().unwrap_or(UNKNOWN_LICENSE),
        identity.architecture.rpm(),
        &identity.description.synopsis,
    );

    // `source_date` fixes the build time and signature stamp. Files added through `with_file`
    // keep their own mtime when it is older than this — see `add_entry`.
    builder
        .using_config(BuildConfig::default().source_date(source_date(plan)))
        .release(&identity.release_rpm)
        .description(full_description(plan))
        .packager(&identity.maintainer);

    if let Some(epoch) = identity.epoch {
        builder.epoch(epoch);
    }
    if let Some(homepage) = &identity.homepage {
        builder.url(homepage);
    }
    for name in dependency_names(identity.dependencies.as_deref()) {
        // What `%pre` itself calls must be `Requires(pre)`; a plain `Requires` does not promise
        // the package is configured that early. Fedora names `shadow-utils` for this.
        let dependency = if scriptlets::runs_before_install(&name) {
            Dependency {
                name: name.clone(),
                flags: rpm::DependencyFlags::SCRIPT_PRE,
                version: String::new(),
            }
        } else {
            Dependency::any(name)
        };
        builder.requires(dependency);
    }

    // Directories included: the plan synthesises every ancestor, and a header listing only
    // files left `/usr/lib/<name>` behind after `rpm --erase`. Which directories the package
    // *owns* is still decided here — RPM does not reference-count shared paths as dpkg does.
    for file in &plan.files {
        if file.kind == EntryKind::Directory && !owns_directory(plan, file.destination.as_str()) {
            continue;
        }
        add_entry(&mut builder, file)?;
    }

    for (name, contents) in &options.maintainer_scripts {
        let script = String::from_utf8_lossy(contents).into_owned();
        match name.as_str() {
            "pre" => {
                builder.pre_install_script(script);
            }
            "post" => {
                builder.post_install_script(script);
            }
            "preun" => {
                builder.pre_uninstall_script(script);
            }
            "postun" => {
                builder.post_uninstall_script(script);
            }
            other => {
                return Err(Error::Unrepresentable {
                    destination: other.to_owned(),
                    reason: "RPM's scriptlet slots are `pre`, `post`, `preun` and `postun`; \
                             this name is none of them"
                        .to_owned(),
                });
            }
        }
    }

    let package = builder
        .build()
        .map_err(|e| Error::rpm("could not assemble the package", e))?;

    Ok(package)
}

/// Builds the package in memory; the `rpm` crate buffers the payload anyway. For tests.
pub fn build_bytes(plan: &BuildPlan, options: &Options) -> Result<Vec<u8>> {
    let package = assemble(plan, options)?;
    let mut bytes = Vec::with_capacity(1 << 16);
    package
        .write(&mut bytes)
        .map_err(|e| Error::rpm("could not serialise the package", e))?;
    Ok(bytes)
}

/// The conventional file name for a package.
#[must_use]
pub fn file_name(plan: &BuildPlan) -> String {
    let identity = &plan.identity;
    format!(
        "{}-{}-{}.{}.rpm",
        identity.package_name,
        identity.version_rpm,
        identity.release_rpm,
        identity.architecture.rpm()
    )
}

/// Roots no package may claim, whatever its name. A backstop for a package *named* after a
/// system directory; the real rule is [`owns_directory`].
const FILESYSTEM_ROOTS: &[&str] = &[
    "/", "/bin", "/boot", "/etc", "/lib", "/lib64", "/opt", "/run", "/sbin", "/srv", "/usr", "/var",
];

/// The plan plus the preset policy for `options.preset_service`, or the plan as it was.
///
/// Rebuilt through [`BuildPlan::new`] so the preset directory is synthesised like any other
/// ancestor and a duplicate destination is refused; [`owns_directory`] leaves it to `systemd`.
fn with_preset(plan: &BuildPlan, options: &Options) -> Result<BuildPlan> {
    let Some(service) = &options.preset_service else {
        return Ok(plan.clone());
    };
    let mut files = plan.files.clone();
    files.push(nativepkg_core::build::systemd_preset_entry(service).map_err(Error::Core)?);
    BuildPlan::new(
        plan.identity.clone(),
        files,
        plan.timestamp,
        plan.metadata.clone(),
    )
    .map_err(Error::Core)
}

/// Whether the package owns a directory rather than merely installing into it.
///
/// `/usr` and `/etc` belong to `filesystem`; RPM, unlike dpkg, does not reference-count shared
/// directories, so claiming them makes two packages own one path. A directory is owned when the
/// package name is one of its components — a literal list of system directories once claimed
/// `/usr/local` under `--install-dir /usr/local/lib`, and `--install-dir` is free-form.
fn owns_directory(plan: &BuildPlan, path: &str) -> bool {
    let package = plan.identity.package_name.as_str();
    !FILESYSTEM_ROOTS.contains(&path) && path.split('/').any(|component| component == package)
}

/// Adds one plan entry to the builder.
///
/// Known limitation: for entries streamed through `with_file` the crate stores
/// `min(source_date, mtime)`, so a source older than the plan's timestamp keeps its own mtime
/// (the Debian backend stamps every entry). `rpm` 0.27 offers no override short of reading
/// each file into memory, which is the buffering that ruled out `arx-pack`. With
/// `SOURCE_DATE_EPOCH` and a later checkout every mtime is newer and the output is
/// machine-independent; tested both ways so a crate release that closes the gap is noticed.
fn add_entry(builder: &mut PackageBuilder, file: &PlannedFile) -> Result<()> {
    let destination = file.destination.as_str();

    match &file.kind {
        EntryKind::Symlink { target } => {
            // The constructor `FileOptions::symlink` sets the file type; the builder method of
            // the same name only records the link text, and the package then installs a
            // zero-byte regular file that looks right in the header.
            builder
                .with_symlink(FileOptions::symlink(destination, target))
                .map_err(|e| Error::rpm(format!("could not add symlink `{destination}`"), e))?;
        }
        EntryKind::Directory => {
            let options = FileOptions::dir(destination).mode(FileMode::dir(mode_bits(file.mode)));
            builder
                .with_dir_entry(options)
                .map_err(|e| Error::rpm(format!("could not add directory `{destination}`"), e))?;
        }
        EntryKind::Regular => {
            let mut options =
                FileOptions::new(destination).mode(FileMode::regular(mode_bits(file.mode)));
            if file.is_config {
                // `noreplace` keeps an administrator's edits across an upgrade, like `conffiles`.
                options = options.config().noreplace();
            }

            match &file.content {
                FileContent::Inline(bytes) => {
                    builder
                        .with_file_contents(bytes.clone(), options)
                        .map_err(|e| Error::rpm(format!("could not add `{destination}`"), e))?;
                }
                FileContent::FromPath { path, .. } => {
                    builder.with_file(path, options).map_err(|e| {
                        Error::rpm(
                            format!("could not add `{destination}` from `{}`", path.display()),
                            e,
                        )
                    })?;
                }
                FileContent::None => {
                    return Err(Error::Unrepresentable {
                        destination: destination.to_owned(),
                        reason: "a regular file with no content is a defect in the plan".to_owned(),
                    });
                }
            }
        }
    }
    Ok(())
}

/// The build timestamp, clamped to RPM's 32-bit epoch field rather than wrapped into the past.
fn source_date(plan: &BuildPlan) -> u32 {
    u32::try_from(plan.timestamp.as_secs()).unwrap_or(u32::MAX)
}

/// Permission bits without file-type bits; the mask leaves twelve bits, so `u16` cannot fail.
fn mode_bits(mode: u32) -> u16 {
    (mode & 0o7777) as u16
}

/// The description RPM shows, synopsis included when there is no body.
fn full_description(plan: &BuildPlan) -> String {
    let description = &plan.identity.description;
    if description.body.is_empty() {
        description.synopsis.clone()
    } else {
        format!("{}\n\n{}", description.synopsis, description.body)
    }
}

/// Splits a Debian-spelling dependency list into bare package names.
///
/// Lossy both ways, and not symmetrically: taking the first of `a | b` narrows the requirement
/// and fails safe; dropping `nodejs (>= 18)` to `nodejs` widens it and fails open. Translating
/// version relations is the first thing to revisit once versioned dependencies are used.
fn dependency_names(expression: Option<&str>) -> Vec<String> {
    expression
        .unwrap_or_default()
        .split(',')
        .filter_map(|entry| {
            let name = entry
                .split('(')
                .next()
                .unwrap_or_default()
                .split('|')
                .next()
                .unwrap_or_default()
                .trim();
            if name.is_empty() {
                None
            } else {
                Some(name.to_owned())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_names_are_extracted_from_the_debian_spelling() {
        assert_eq!(
            dependency_names(Some("nodejs, redis-server")),
            vec!["nodejs", "redis-server"]
        );
    }

    #[test]
    fn version_relations_are_dropped_rather_than_mistranslated() {
        assert_eq!(
            dependency_names(Some("nodejs (>= 18), libc6 (>= 2.34)")),
            vec!["nodejs", "libc6"]
        );
    }

    #[test]
    fn alternatives_take_the_first_named() {
        assert_eq!(
            dependency_names(Some("nodejs | nodejs-legacy")),
            vec!["nodejs"]
        );
    }

    #[test]
    fn an_absent_or_empty_expression_yields_nothing() {
        assert!(dependency_names(None).is_empty());
        assert!(dependency_names(Some("   ")).is_empty());
        assert!(dependency_names(Some(",,")).is_empty());
    }

    #[test]
    fn mode_bits_drop_file_type_bits() {
        assert_eq!(mode_bits(0o100_644), 0o644);
        assert_eq!(mode_bits(0o755), 0o755);
    }
}
