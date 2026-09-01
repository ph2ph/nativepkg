//! The target formats, and how a plan becomes a package in each.
//!
//! The only place in the workspace that knows all three formats: `nativepkg-core` has no notion
//! of a target format (two of its tests enforce that) and each backend sees only itself.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use nativepkg_core::npm::InitSystem;
use nativepkg_core::plan::BuildPlan;
use nativepkg_core::resolve::ResolvedConfig;
use nativepkg_core::template::Variables;

/// A package format `nativepkg` can produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
pub enum Format {
    Deb,
    Rpm,
    Arch,
}

impl Format {
    pub const ALL: [Self; 3] = [Self::Deb, Self::Rpm, Self::Arch];

    /// The name accepted on the command line.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Deb => "deb",
            Self::Rpm => "rpm",
            Self::Arch => "arch",
        }
    }

    /// The version spelling this format writes into its own metadata. Templates are rendered
    /// per format because of this: one spelling for all is how a package contradicted its own
    /// header.
    #[must_use]
    pub fn version_of(self, config: &ResolvedConfig) -> String {
        match self {
            Self::Deb => config.version.deb().to_owned(),
            // RPM and Arch carry the epoch outside the version string.
            Self::Rpm | Self::Arch => config.version.rpm_version().to_owned(),
        }
    }

    /// The architecture spelling this format writes into its own metadata.
    pub fn architecture_of(self, config: &ResolvedConfig) -> Result<&'static str> {
        let parsed = config
            .architecture_parsed()
            .context("architecture is not one this tool recognises")?;
        Ok(match self {
            Self::Deb => parsed.deb(),
            Self::Rpm => parsed.rpm(),
            Self::Arch => parsed.arch_linux(),
        })
    }

    /// The template vocabulary for this format.
    pub fn variables(self, config: &ResolvedConfig, generator_version: &str) -> Result<Variables> {
        Ok(Variables::for_config(
            config,
            generator_version,
            &self.version_of(config),
            self.architecture_of(config)?,
        ))
    }

    /// What this format cannot honour about the configuration, so the user hears it rather
    /// than getting a package that installs cleanly and never starts its service.
    #[must_use]
    pub fn unsupported(self, init: InitSystem) -> Option<String> {
        match self {
            Self::Arch => nativepkg_arch::install::unsupported_init(init).map(|name| {
                format!(
                    "Arch Linux has been systemd-only for over a decade, so `init: {name}` \
                     cannot be honoured; the package will carry no service lifecycle"
                )
            }),
            Self::Rpm if init == InitSystem::Upstart => Some(
                "Upstart shipped on RHEL 6, which is long past end of life; `init: upstart` \
                 cannot be honoured for RPM"
                    .to_owned(),
            ),
            Self::Deb | Self::Rpm => None,
        }
    }

    /// Builds this format's package, returning the path written.
    pub fn build(
        self,
        plan: &BuildPlan,
        config: &ResolvedConfig,
        variables: &Variables,
        output_dir: &Path,
    ) -> Result<PathBuf> {
        let init = config.init;
        let strategy = config.install_strategy;

        let path = match self {
            Self::Deb => {
                let mut scripts = Vec::new();
                for script in nativepkg_deb::scripts::Script::all() {
                    let text = nativepkg_deb::scripts::compose(script, init, strategy, variables)
                        .with_context(|| format!("composing `{}`", script.name()))?;
                    if has_logic(&text) {
                        scripts.push((script.name().to_owned(), text.into_bytes()));
                    }
                }
                // What the scripts call becomes a dependency: a `-slim` image has no `adduser`,
                // and `postinst` exits 127.
                let required = nativepkg_deb::scripts::required_packages(init, strategy);
                let plan = with_dependencies(plan, &required);

                let triggers = match &config.triggers_file {
                    Some(path) => Some(
                        std::fs::read(path)
                            .with_context(|| format!("reading triggers file {}", path.display()))?,
                    ),
                    None => None,
                };
                let options = nativepkg_deb::Options {
                    maintainer_scripts: scripts,
                    triggers,
                    ..nativepkg_deb::Options::default()
                };
                nativepkg_deb::build(&plan, &options, output_dir).context("writing the .deb")?
            }
            Self::Rpm => {
                let mut scripts = Vec::new();
                for slot in nativepkg_rpm::scriptlets::Scriptlet::all() {
                    let text = nativepkg_rpm::scriptlets::compose(slot, init, strategy, variables)
                        .with_context(|| format!("composing `%{}`", slot.name()))?;
                    if has_logic(&text) {
                        scripts.push((slot.name().to_owned(), text.into_bytes()));
                    }
                }
                // Same as the Debian arm: what the scriptlets call, the package requires.
                let required = nativepkg_rpm::scriptlets::required_packages(init, strategy);
                let plan = with_dependencies(plan, &required);

                // Ship a preset policy exactly when `%post` presets the unit. Fedora's default
                // is `disable *`, so a preset with no policy behind it is a service that never
                // comes up.
                let options = nativepkg_rpm::Options {
                    maintainer_scripts: scripts,
                    preset_service: nativepkg_rpm::scriptlets::uses_preset(init)
                        .then(|| plan.identity.package_name.clone()),
                };
                nativepkg_rpm::build(&plan, &options, output_dir).context("writing the .rpm")?
            }
            Self::Arch => {
                let scriptlet = nativepkg_arch::install::compose(init, strategy, variables)
                    .context("composing `.INSTALL`")?;
                let install_scriptlet = scriptlet.map(String::into_bytes);
                // Same coupling as the RPM arm: Arch's `99-default.preset` is `disable *`.
                let options = nativepkg_arch::Options {
                    install_scriptlet,
                    preset_service: nativepkg_arch::install::uses_preset(init)
                        .then(|| plan.identity.package_name.clone()),
                };
                nativepkg_arch::build(plan, &options, output_dir)
                    .context("writing the .pkg.tar.zst")?
            }
        };

        Ok(path)
    }
}

/// A copy of the plan whose dependencies also name what its maintainer scripts need. The
/// shared plan is left alone: the other formats' scripts need different things.
fn with_dependencies<'a>(plan: &'a BuildPlan, required: &[&str]) -> Cow<'a, BuildPlan> {
    if required.is_empty() {
        return Cow::Borrowed(plan);
    }

    let mut merged: Vec<String> = plan
        .identity
        .dependencies
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .map(|entry| entry.trim().to_owned())
        .filter(|entry| !entry.is_empty())
        .collect();

    for package in required {
        // Compare on the name alone, so a version constraint the user wrote is left as is.
        if !merged
            .iter()
            .any(|entry| entry.split_whitespace().next() == Some(*package))
        {
            merged.push((*package).to_owned());
        }
    }

    let mut plan = plan.clone();
    plan.identity.dependencies = Some(merged.join(", "));
    Cow::Owned(plan)
}

/// Whether a composed script does anything beyond shebang, generator comment and `exit 0`.
/// Shipping one that does not is a root-run no-op in the archive, and a lintian complaint.
fn has_logic(text: &str) -> bool {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .any(|line| !line.starts_with('#') && !line.starts_with("set -e") && line != "exit 0")
}

impl core::fmt::Display for Format {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.name())
    }
}
