//! The single precedence chain that turns the settings plus overrides into a validated
//! configuration. Highest first:
//!
//! 1. command-line overrides
//! 2. the `.nativepkg` settings
//! 3. built-in defaults
//!
//! Implemented once, in `Layers::pick`, and applied to every field. `package.json` is not a
//! source; the engine still carries a `Manifest` layer, but the CLI never populates it.

use core::fmt;
use std::path::PathBuf;

use crate::name::{EntryPoint, ExecutableName, InstallDir, PackageName, UnixName};
use crate::npm::{
    Author, Entrypoints, InitSystem, InstallStrategy, Manifest, Settings, Templates, command_binary,
};
use crate::text;
use crate::version::{MappedVersion, VersionSpec};
use crate::{Error, Result};

/// The dependency `--nodejs` adds, for a Node.js application. Nothing is added by default:
/// the tool packages any language, so a `nodejs` dependency is opt-in, not assumed.
const NODEJS_DEPENDENCY: &str = "nodejs";

/// `/usr/lib`, not bash's `/usr/share`: the FHS reserves `/usr/share` for
/// architecture-independent data, and `node_modules` routinely contains compiled addons.
const DEFAULT_INSTALL_DIR: &str = "/usr/lib";

const DEFAULT_ARCHITECTURE: &str = "all";

/// Values supplied on the command line; absent means "defer".
///
/// Deliberately not `#[non_exhaustive]`: it is built by `nativepkg-cli` and by tests in other
/// crates, and `#[non_exhaustive]` forbids struct-literal syntax there — including
/// `..Default::default()` (E0639).
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    /// Validated strictly, never normalised.
    pub package_name: Option<String>,
    pub version: Option<String>,
    pub epoch: Option<u32>,
    pub description: Option<String>,
    pub maintainer: Option<String>,
    pub architecture: Option<String>,
    pub dependencies: Option<String>,
    pub install_dir: Option<String>,
    pub user: Option<String>,
    pub group: Option<String>,
    pub executable_name: Option<String>,
    pub output_deb_name: Option<String>,
    pub extra_files: Option<String>,
    pub triggers_file: Option<String>,
    pub daemon_entrypoint: Option<String>,
    pub cli_entrypoint: Option<String>,
    pub init: Option<InitSystem>,
    pub install_strategy: Option<InstallStrategy>,
    pub install_command: Option<String>,
    pub install_binary: Option<String>,
    /// Add `nodejs` to the dependencies; off unless the caller asks for a Node.js application.
    pub include_nodejs: bool,
}

/// A fully resolved, validated configuration; downstream stages do not re-validate.
/// `#[non_exhaustive]` so only [`resolve`] can construct one.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ResolvedConfig {
    pub package_name: PackageName,
    pub version: MappedVersion,
    /// As written by the user; splitting into synopsis and body is a control-file concern.
    pub description: String,
    pub maintainer: String,
    /// As supplied; see [`ResolvedConfig::architecture_parsed`]. Kept as text so resolution
    /// stays free of format concerns.
    pub architecture: String,
    /// npm workspace roots, relative to the project root. Only these may be materialised when
    /// linked into `node_modules`; anything else a dependency links to is refused, since
    /// `node_modules` is untrusted input.
    pub workspace_roots: Vec<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    pub dependencies: Option<String>,
    pub install_dir: InstallDir,
    pub user: UnixName,
    pub group: UnixName,
    pub executable_name: ExecutableName,
    pub init: InitSystem,
    pub install_strategy: InstallStrategy,
    /// The resolved install-at-unpack command, and the binary its guard checks for.
    pub install_command: String,
    pub install_binary: String,
    /// Directory copied verbatim to the filesystem root.
    pub extra_files: Option<PathBuf>,
    pub triggers_file: Option<PathBuf>,
    pub output_deb_name: Option<String>,
    /// Required unless [`InitSystem::None`].
    pub daemon_entrypoint: Option<EntryPoint>,
    /// Defaults to the daemon entrypoint.
    pub cli_entrypoint: Option<EntryPoint>,
    pub templates: Templates,
}

impl ResolvedConfig {
    /// Parses the architecture for a backend to render. Never defaulted: an unknown
    /// architecture is an error, not a package claiming to run where it cannot.
    pub fn architecture_parsed(&self) -> Result<crate::arch::Architecture> {
        crate::arch::Architecture::parse(&self.architecture)
    }
}

/// A non-fatal condition worth telling the user about. Returned rather than logged, so tests
/// can assert on it and the crate imposes no logging framework.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Warning {
    PackageNameNormalised {
        original: String,
        normalised: String,
    },
    /// Not semver, so no `~` mapping was applied.
    VersionNotMapped {
        version: String,
    },
    /// A `-` inside a semver identifier was rewritten to `.`. Carries only the version as
    /// supplied: the rewritten form differs per format (Debian prefixes the epoch, RPM does
    /// not) and the core does not know which package is being built.
    VersionHyphensRewritten {
        original: String,
    },
    /// Reported rather than ignored: serde's default is to skip unknown fields, so a typo
    /// produced a build that quietly did not honour the manifest.
    UnknownSettingKey {
        key: String,
        suggestion: Option<&'static str>,
        object: &'static str,
    },
    DependenciesExcluded {
        reason: String,
    },
    DependenciesMayIncludeDevelopmentPackages,
    DependenciesInstalledAtInstallTime,
    CompiledAddonsInArchitectureIndependentPackage {
        example: String,
    },
    UnixNameDerived {
        kind: &'static str,
        source: String,
        derived: String,
    },
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PackageNameNormalised {
                original,
                normalised,
            } => write!(
                f,
                "package name `{original}` is not valid for native packaging; using `{normalised}`"
            ),
            Self::VersionNotMapped { version } => write!(
                f,
                "version `{version}` is not semver; using it verbatim without pre-release mapping"
            ),
            Self::VersionHyphensRewritten { original } => write!(
                f,
                "version `{original}` contains `-` inside an identifier, which no native \
                 package version can carry; it was rewritten for the target format"
            ),
            Self::UnknownSettingKey {
                key,
                suggestion,
                object,
            } => match suggestion {
                Some(near) => write!(
                    f,
                    "`{object}.{key}` is not a setting and was ignored; did you mean `{near}`?"
                ),
                None => write!(f, "`{object}.{key}` is not a setting and was ignored"),
            },
            Self::DependenciesExcluded { reason } => {
                write!(f, "`node_modules` was not included: {reason}")
            }
            Self::DependenciesInstalledAtInstallTime => write!(
                f,
                "the install-time strategy was selected: installing this package will require \
                 network access and will run third-party install scripts as root on the target \
                 machine. The default strategy vendors dependencies at build time instead"
            ),
            Self::CompiledAddonsInArchitectureIndependentPackage { example } => write!(
                f,
                "the vendored dependencies contain a compiled addon (`{example}`) but the \
                 package declares architecture `all`. Compiled addons are specific to an \
                 architecture and to a Node.js ABI, so this package will not work everywhere \
                 it claims to; set an explicit architecture, or build on the target platform"
            ),
            Self::DependenciesMayIncludeDevelopmentPackages => write!(
                f,
                "the vendored `node_modules` is packaged as it exists on disk and may contain \
                 development dependencies; run `npm ci --omit=dev` before packaging to avoid \
                 shipping them"
            ),
            Self::UnixNameDerived {
                kind,
                source,
                derived,
            } => write!(
                f,
                "`{source}` is not a valid {kind} name; using `{derived}`"
            ),
        }
    }
}

/// The three configuration layers; [`Layers::pick`] is the entire precedence implementation.
struct Layers<'a> {
    overrides: &'a Overrides,
    primary: Option<&'a Settings>,
    manifest: &'a Manifest,
}

impl Layers<'_> {
    /// CLI, then `nativepkg`, then the top-level manifest field.
    fn pick<T>(
        &self,
        from_cli: Option<T>,
        from_settings: impl Fn(&Settings) -> Option<T>,
        from_manifest: Option<T>,
    ) -> Option<T> {
        from_cli
            .or_else(|| self.primary.and_then(&from_settings))
            .or(from_manifest)
    }

    /// For a field that exists only in the settings objects and on the command line.
    fn pick_setting<T>(
        &self,
        from_cli: Option<T>,
        from_settings: impl Fn(&Settings) -> Option<T>,
    ) -> Option<T> {
        self.pick(from_cli, from_settings, None)
    }

    /// A whole settings object from the `nativepkg` layer, or its default.
    fn merged<T: Default>(&self, get: impl Fn(&Settings) -> Option<T>) -> T {
        self.primary.and_then(get).unwrap_or_default()
    }
}

/// Where the package lands and who runs it.
struct Placement {
    architecture: String,
    install_dir: InstallDir,
    user: UnixName,
    group: UnixName,
    executable_name: ExecutableName,
}

/// How the package behaves once installed.
struct Runtime {
    init: InitSystem,
    install_strategy: InstallStrategy,
    daemon_entrypoint: Option<EntryPoint>,
    cli_entrypoint: Option<EntryPoint>,
}

/// Resolves a manifest and command-line overrides into a validated configuration plus any
/// non-fatal warnings.
///
/// # Errors
///
/// [`Error::InvalidPackageName`] or [`Error::InvalidVersion`] when an identity field cannot be
/// made valid; [`Error::Manifest`] when a required field is absent from every layer.
/// The install-at-unpack command when the caller sets none: plain npm, the one manager present
/// wherever `nodejs` is. Any other manager is named explicitly with `--install-command`.
const DEFAULT_INSTALL_COMMAND: &str =
    "npm install --omit=dev --ignore-scripts --no-audit --no-fund";

/// The install-at-unpack command and the binary its guard checks. A caller override or a
/// `.nativepkg` setting wins; otherwise it is plain npm, and the guard binary is derived from the
/// command. Both are checked for characters that would break the script.
fn resolve_install(layers: &Layers<'_>) -> Result<(String, String)> {
    let o = layers.overrides;
    let command = layers
        .pick_setting(o.install_command.clone(), |s| s.install_command.clone())
        .unwrap_or_else(|| DEFAULT_INSTALL_COMMAND.to_owned());
    text::single_line("install_command", &command)?;
    let binary = layers
        .pick_setting(o.install_binary.clone(), |s| s.install_binary.clone())
        .unwrap_or_else(|| command_binary(&command).unwrap_or("npm").to_owned());
    text::token("install_binary", &binary)?;
    Ok((command, binary))
}

pub fn resolve(
    manifest: &Manifest,
    overrides: &Overrides,
) -> Result<(ResolvedConfig, Vec<Warning>)> {
    let layers = Layers {
        overrides,
        primary: manifest.nativepkg.as_ref(),
        manifest,
    };

    let (install_command, install_binary) = resolve_install(&layers)?;

    let mut warnings = Vec::new();
    // A typo in the settings object is silent otherwise: serde skips unknown fields.
    if let Some(settings) = layers.primary {
        for (key, suggestion) in settings.unknown_keys() {
            warnings.push(Warning::UnknownSettingKey {
                key,
                suggestion,
                object: "nativepkg",
            });
        }
    }

    let package_name = resolve_package_name(&layers, &mut warnings)?;
    let version = resolve_version(&layers, &mut warnings)?;
    let placement = resolve_placement(&layers, &package_name, &mut warnings)?;
    let runtime = resolve_runtime(&layers)?;

    let description = layers
        .pick(
            overrides.description.clone(),
            |s| s.description.clone(),
            manifest.description.clone(),
        )
        .ok_or_else(|| missing("description"))?;

    let maintainer = layers
        .pick(
            overrides.maintainer.clone(),
            |s| s.maintainer.clone(),
            manifest.author.as_ref().map(Author::to_maintainer),
        )
        .ok_or_else(|| missing("maintainer"))?;

    text::printable("description", &description)?;
    text::single_line("maintainer", &maintainer)?;
    let homepage = layers.pick(None, |s| s.homepage.clone(), manifest.homepage.clone());
    if let Some(homepage) = &homepage {
        text::token("homepage", homepage)?;
    }
    let license = layers.pick(None, |s| s.license.clone(), manifest.license.clone());
    if let Some(license) = &license {
        text::single_line("license", license)?;
    }
    let dependencies = resolve_dependencies(
        layers.pick_setting(overrides.dependencies.clone(), |s| s.dependencies.clone()),
        overrides.include_nodejs,
    );
    if let Some(dependencies) = &dependencies {
        text::single_line("dependencies", dependencies)?;
    }

    Ok((
        ResolvedConfig {
            package_name,
            version,
            description,
            maintainer,
            architecture: placement.architecture,
            workspace_roots: manifest
                .workspaces
                .as_ref()
                .map(crate::npm::Workspaces::directory_prefixes)
                .unwrap_or_default(),
            homepage,
            license,
            dependencies,
            install_dir: placement.install_dir,
            user: placement.user,
            group: placement.group,
            executable_name: placement.executable_name,
            init: runtime.init,
            install_strategy: runtime.install_strategy,
            install_command,
            install_binary,
            extra_files: layers
                .pick_setting(overrides.extra_files.clone(), |s| s.extra_files.clone())
                .map(PathBuf::from),
            triggers_file: layers
                .pick_setting(overrides.triggers_file.clone(), |s| s.triggers_file.clone())
                .map(PathBuf::from),
            output_deb_name: layers.pick_setting(overrides.output_deb_name.clone(), |s| {
                s.output_deb_name.clone()
            }),
            daemon_entrypoint: runtime.daemon_entrypoint,
            cli_entrypoint: runtime.cli_entrypoint,
            templates: layers.merged(|s| s.templates.clone()),
        },
        warnings,
    ))
}

/// Resolves architecture, install root and the service account.
fn resolve_placement(
    layers: &Layers<'_>,
    package_name: &PackageName,
    warnings: &mut Vec<Warning>,
) -> Result<Placement> {
    let o = layers.overrides;

    let architecture = layers
        .pick_setting(o.architecture.clone(), |s| s.architecture.clone())
        .unwrap_or_else(|| DEFAULT_ARCHITECTURE.to_owned());

    let install_dir = layers
        .pick_setting(o.install_dir.clone(), |s| s.install_dir.clone())
        .unwrap_or_else(|| DEFAULT_INSTALL_DIR.to_owned());

    let user = resolve_account(
        "user",
        layers.pick_setting(o.user.clone(), |s| s.user.clone()),
        package_name.as_str(),
        warnings,
    )?;

    let group = match layers.pick_setting(o.group.clone(), |s| s.group.clone()) {
        Some(explicit) => UnixName::parse_strict("group", &explicit)?,
        // The user name is already a validated `UnixName`, so no derivation or warning.
        None => user.clone(),
    };

    let executable_name = layers
        .pick_setting(o.executable_name.clone(), |s| s.executable_name.clone())
        .unwrap_or_else(|| package_name.as_str().to_owned());

    Ok(Placement {
        architecture,
        install_dir: InstallDir::parse(&install_dir)?,
        user,
        group,
        executable_name: ExecutableName::parse(&executable_name)?,
    })
}

/// Resolves init integration, dependency strategy and entry points.
fn resolve_runtime(layers: &Layers<'_>) -> Result<Runtime> {
    let o = layers.overrides;

    let init = layers.pick_setting(o.init, |s| s.init).unwrap_or_default();
    let install_strategy = layers
        .pick_setting(o.install_strategy, |s| s.install_strategy)
        .unwrap_or_default();

    // The command line outranks both manifest layers here as everywhere else; before, the
    // entry point could only be set in `package.json`.
    let Entrypoints { daemon, cli } = layers.merged(|s| s.entrypoints.clone());
    let daemon = layers.overrides.daemon_entrypoint.clone().or(daemon);
    let cli = layers.overrides.cli_entrypoint.clone().or(cli);
    let cli_entrypoint = cli.or_else(|| daemon.clone());

    if init != InitSystem::None && daemon.is_none() {
        return Err(Error::manifest(
            "a daemon entrypoint is required unless `init` is `none`; pass \
             `--daemon <file>` or set `entrypoints.daemon` in `.nativepkg`",
        ));
    }

    Ok(Runtime {
        init,
        install_strategy,
        daemon_entrypoint: daemon.as_deref().map(EntryPoint::parse).transpose()?,
        cli_entrypoint: cli_entrypoint
            .as_deref()
            .map(EntryPoint::parse)
            .transpose()?,
    })
}

/// Builds the error for a required field that no layer supplied.
fn missing(what: &str) -> Error {
    Error::manifest(format!(
        "no {what} could be resolved; set it on the command line or in the `.nativepkg` config"
    ))
}

/// Strict when explicit, derived from the package name otherwise. A package name is a looser
/// grammar than an account name (`0ad` is a real package; `.` and `+` are legal) and
/// `adduser --system` rejects all of that; bash copied it unchecked and failed at install time.
fn resolve_account(
    kind: &'static str,
    explicit: Option<String>,
    fallback_source: &str,
    warnings: &mut Vec<Warning>,
) -> Result<UnixName> {
    if let Some(name) = explicit {
        return UnixName::parse_strict(kind, &name);
    }
    let derived = UnixName::derive_from(kind, fallback_source)?;
    if derived.as_str() != fallback_source {
        warnings.push(Warning::UnixNameDerived {
            kind,
            source: fallback_source.to_owned(),
            derived: derived.as_str().to_owned(),
        });
    }
    Ok(derived)
}

/// Explicit values are validated strictly; only the npm name is normalised.
fn resolve_package_name(layers: &Layers<'_>, warnings: &mut Vec<Warning>) -> Result<PackageName> {
    let explicit = layers.pick_setting(layers.overrides.package_name.clone(), |s| {
        s.package_name.clone()
    });

    if let Some(name) = explicit {
        // The user named it deliberately, so it is validated, never rewritten.
        return PackageName::parse_strict(&name);
    }

    let npm_name = layers
        .manifest
        .name
        .clone()
        .ok_or_else(|| missing("package name"))?;
    let normalised = PackageName::normalize(&npm_name)?;
    if normalised.as_str() != npm_name {
        warnings.push(Warning::PackageNameNormalised {
            original: npm_name,
            normalised: normalised.as_str().to_owned(),
        });
    }
    Ok(normalised)
}

/// Resolves and maps the version.
fn resolve_version(layers: &Layers<'_>, warnings: &mut Vec<Warning>) -> Result<MappedVersion> {
    let raw = layers
        .pick(
            layers.overrides.version.clone(),
            |s| s.version.clone(),
            layers.manifest.version.clone(),
        )
        .ok_or_else(|| missing("version"))?;

    let spec = VersionSpec::parse(&raw)?;
    if spec.is_literal() {
        warnings.push(Warning::VersionNotMapped {
            version: raw.clone(),
        });
    }

    let epoch = layers.pick_setting(layers.overrides.epoch, |s| s.epoch);
    let mapped = MappedVersion::new(&spec, epoch)?;

    if mapped.hyphens_rewritten() {
        warnings.push(Warning::VersionHyphensRewritten { original: raw });
    }
    Ok(mapped)
}

/// The package depends on exactly what the caller declares. `nodejs` is prepended only when
/// asked for, since the tool is not Node-specific.
fn resolve_dependencies(user_supplied: Option<String>, include_nodejs: bool) -> Option<String> {
    match (include_nodejs, user_supplied) {
        (false, user) => user,
        (true, Some(user)) => Some(format!("{NODEJS_DEPENDENCY}, {user}")),
        (true, None) => Some(NODEJS_DEPENDENCY.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    /// The entry point used to be settable only in `package.json`, so packaging a project you
    /// do not own meant editing its manifest first.
    #[test]
    fn entrypoints_can_come_from_the_command_line_alone() {
        let overrides = Overrides {
            daemon_entrypoint: Some("index.js".into()),
            ..Overrides::default()
        };
        let (cfg, _) = resolve_ok(
            r#"{"name":"app","version":"1.0.0","description":"d","author":"A <a@example.com>"}"#,
            &overrides,
        );
        assert_eq!(cfg.daemon_entrypoint.as_deref(), Some("index.js"));
        assert_eq!(
            cfg.cli_entrypoint.as_deref(),
            Some("index.js"),
            "cli falls back to daemon, from the override too"
        );
    }

    #[test]
    fn a_command_line_entrypoint_outranks_the_manifest() {
        let overrides = Overrides {
            daemon_entrypoint: Some("from-cli.js".into()),
            cli_entrypoint: Some("cli-from-cli.js".into()),
            ..Overrides::default()
        };
        let (cfg, _) = resolve_ok(
            r#"{"name":"app","version":"1.0.0","description":"d","author":"A <a@example.com>",
                "nativepkg":{"entrypoints":{"daemon":"from-manifest.js","cli":"cli-from-manifest.js"}}}"#,
            &overrides,
        );
        assert_eq!(cfg.daemon_entrypoint.as_deref(), Some("from-cli.js"));
        assert_eq!(cfg.cli_entrypoint.as_deref(), Some("cli-from-cli.js"));
    }

    use super::*;

    fn manifest(json: &str) -> Manifest {
        serde_json::from_str(json).expect("test fixture should deserialise")
    }

    fn resolve_ok(json: &str, overrides: &Overrides) -> (ResolvedConfig, Vec<Warning>) {
        resolve(&manifest(json), overrides).expect("fixture should resolve")
    }

    /// A manifest with everything required, so tests can vary one thing at a time.
    const COMPLETE: &str = r#"{
        "name": "simple",
        "version": "1.2.3",
        "description": "a description",
        "author": "Someone <s@example.com>",
        "nativepkg": { "init": "none" }
    }"#;

    #[test]
    fn command_line_wins_over_settings_objects() {
        let json = r#"{
            "name": "manifest-name", "version": "1.0.0", "description": "d",
            "author": "a", "nativepkg": { "package_name": "obj-name", "init": "none" }
        }"#;
        let overrides = Overrides {
            package_name: Some("cli-name".into()),
            ..Overrides::default()
        };
        let (cfg, _) = resolve_ok(json, &overrides);
        assert_eq!(cfg.package_name.as_str(), "cli-name");
    }

    #[test]
    fn a_custom_install_command_overrides_the_default_and_derives_its_binary() {
        let json = r#"{
            "name": "app", "version": "1.0.0", "description": "d", "author": "a",
            "nativepkg": { "init": "none" }
        }"#;
        let overrides = Overrides {
            install_command: Some("YARN_ENABLE_SCRIPTS=false yarn install --immutable".to_owned()),
            ..Overrides::default()
        };
        let (cfg, _) = resolve_ok(json, &overrides);
        assert_eq!(
            cfg.install_command,
            "YARN_ENABLE_SCRIPTS=false yarn install --immutable"
        );
        assert_eq!(
            cfg.install_binary, "yarn",
            "the guard binary is derived from the command"
        );
    }

    #[test]
    fn a_newline_in_a_custom_install_command_is_refused() {
        let json = r#"{
            "name": "app", "version": "1.0.0", "description": "d", "author": "a",
            "nativepkg": { "init": "none" }
        }"#;
        let overrides = Overrides {
            install_command: Some("npm ci\nrm -rf /".to_owned()),
            ..Overrides::default()
        };
        assert!(resolve(&manifest(json), &overrides).is_err());
    }

    #[test]
    fn top_level_fields_are_the_fallback() {
        let (cfg, _) = resolve_ok(COMPLETE, &Overrides::default());
        assert_eq!(cfg.description, "a description");
        assert_eq!(cfg.maintainer, "Someone <s@example.com>");
        assert_eq!(cfg.version.deb(), "1.2.3");
    }

    #[test]
    fn a_missing_required_value_points_at_the_command_line_and_dot_nativepkg() {
        let json = r#"{ "version": "1.0.0", "description": "d", "author": "a" }"#;
        let err = resolve(&manifest(json), &Overrides::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("package name"), "{err}");
        assert!(err.contains(".nativepkg"), "{err}");
    }

    #[test]
    fn missing_maintainer_is_an_error() {
        let json = r#"{ "name": "app", "version": "1.0.0", "description": "d" }"#;
        assert!(resolve(&manifest(json), &Overrides::default()).is_err());
    }

    #[test]
    fn scoped_manifest_name_is_normalised_with_a_warning() {
        let json = r#"{
            "name": "@acme/probe-app", "version": "1.0.0", "description": "d",
            "author": "a", "nativepkg": { "init": "none" }
        }"#;
        let (cfg, warnings) = resolve_ok(json, &Overrides::default());
        assert_eq!(cfg.package_name.as_str(), "acme-probe-app");
        assert!(warnings.contains(&Warning::PackageNameNormalised {
            original: "@acme/probe-app".into(),
            normalised: "acme-probe-app".into(),
        }));
    }

    #[test]
    fn valid_manifest_name_produces_no_warning() {
        let (_, warnings) = resolve_ok(COMPLETE, &Overrides::default());
        assert!(
            !warnings
                .iter()
                .any(|w| matches!(w, Warning::PackageNameNormalised { .. }))
        );
    }

    #[test]
    fn explicit_name_is_validated_not_normalised() {
        let overrides = Overrides {
            package_name: Some("MyApp".into()),
            ..Overrides::default()
        };
        let err = resolve(&manifest(COMPLETE), &overrides).unwrap_err();
        assert!(
            matches!(err, Error::InvalidPackageName { .. }),
            "an explicit invalid name must fail loudly rather than be rewritten"
        );
    }

    #[test]
    fn prerelease_version_is_mapped_and_not_warned_about() {
        let overrides = Overrides {
            version: Some("2.0.0-beta.1".into()),
            ..Overrides::default()
        };
        let (cfg, warnings) = resolve_ok(COMPLETE, &overrides);
        assert_eq!(cfg.version.deb(), "2.0.0~beta.1");
        assert!(
            !warnings
                .iter()
                .any(|w| matches!(w, Warning::VersionNotMapped { .. }))
        );
    }

    #[test]
    fn non_semver_version_passes_through_with_a_warning() {
        let overrides = Overrides {
            version: Some("1.2.3~rc1".into()),
            ..Overrides::default()
        };
        let (cfg, warnings) = resolve_ok(COMPLETE, &overrides);
        assert_eq!(cfg.version.deb(), "1.2.3~rc1");
        assert!(warnings.contains(&Warning::VersionNotMapped {
            version: "1.2.3~rc1".into()
        }));
    }

    #[test]
    fn epoch_is_applied_from_the_settings_object() {
        let json = r#"{
            "name": "app", "version": "1.0.0", "description": "d", "author": "a",
            "nativepkg": { "init": "none", "epoch": 2 }
        }"#;
        let (cfg, _) = resolve_ok(json, &Overrides::default());
        assert_eq!(cfg.version.deb(), "2:1.0.0");
        assert_eq!(cfg.version.epoch(), Some(2));
    }

    #[test]
    fn user_and_group_default_to_the_package_name() {
        let (cfg, _) = resolve_ok(COMPLETE, &Overrides::default());
        assert_eq!(cfg.user.as_str(), "simple");
        assert_eq!(cfg.group.as_str(), "simple");
    }

    #[test]
    fn group_defaults_to_the_resolved_user() {
        let overrides = Overrides {
            user: Some("svc".into()),
            ..Overrides::default()
        };
        let (cfg, _) = resolve_ok(COMPLETE, &overrides);
        assert_eq!(cfg.group.as_str(), "svc");
    }

    #[test]
    fn over_long_unix_names_are_rejected() {
        let overrides = Overrides {
            user: Some("u".repeat(33)),
            ..Overrides::default()
        };
        assert!(resolve(&manifest(COMPLETE), &overrides).is_err());
    }

    #[test]
    fn defaults_match_the_bash_implementation() {
        let (cfg, _) = resolve_ok(COMPLETE, &Overrides::default());
        assert_eq!(cfg.architecture, "all");
        assert_eq!(cfg.executable_name.as_str(), "simple");
        assert_eq!(cfg.install_strategy, InstallStrategy::Auto);
    }

    /// The one default that deliberately does not match bash, kept on the record.
    #[test]
    fn the_install_root_deliberately_diverges_from_bash() {
        let (cfg, _) = resolve_ok(COMPLETE, &Overrides::default());
        assert_eq!(
            cfg.install_dir,
            InstallDir::parse("/usr/lib").expect("valid"),
            "bash used /usr/share, which the FHS reserves for architecture-independent data"
        );
    }

    #[test]
    fn init_defaults_to_auto_when_no_layer_sets_it() {
        let json = r#"{
            "name": "app", "version": "1.0.0", "description": "d", "author": "a",
            "nativepkg": { "entrypoints": { "daemon": "app.js" } }
        }"#;
        let (cfg, _) = resolve_ok(json, &Overrides::default());
        assert_eq!(cfg.init, InitSystem::Auto);
    }

    #[test]
    fn cli_entrypoint_falls_back_to_the_daemon_entrypoint() {
        let json = r#"{
            "name": "app", "version": "1.0.0", "description": "d", "author": "a",
            "nativepkg": { "init": "systemd", "entrypoints": { "daemon": "app.js" } }
        }"#;
        let (cfg, _) = resolve_ok(json, &Overrides::default());
        assert_eq!(cfg.cli_entrypoint.as_deref(), Some("app.js"));
    }

    #[test]
    fn daemon_entrypoint_is_required_unless_init_is_none() {
        let json = r#"{
            "name": "app", "version": "1.0.0", "description": "d", "author": "a",
            "nativepkg": { "init": "systemd" }
        }"#;
        let err = resolve(&manifest(json), &Overrides::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("daemon entrypoint"), "{err}");
    }

    /// The tool packages any language, so nothing is depended on unless the caller says so.
    #[test]
    fn no_dependency_is_added_by_default() {
        let (cfg, _) = resolve_ok(COMPLETE, &Overrides::default());
        assert!(cfg.dependencies.is_none());
    }

    #[test]
    fn user_dependencies_stand_alone_without_nodejs() {
        let overrides = Overrides {
            dependencies: Some("redis-server".into()),
            ..Overrides::default()
        };
        let (cfg, _) = resolve_ok(COMPLETE, &overrides);
        assert_eq!(cfg.dependencies.as_deref(), Some("redis-server"));
    }

    #[test]
    fn nodejs_is_added_only_when_requested() {
        let overrides = Overrides {
            include_nodejs: true,
            dependencies: Some("redis-server".into()),
            ..Overrides::default()
        };
        let (cfg, _) = resolve_ok(COMPLETE, &overrides);
        assert_eq!(cfg.dependencies.as_deref(), Some("nodejs, redis-server"));

        let overrides = Overrides {
            include_nodejs: true,
            ..Overrides::default()
        };
        let (cfg, _) = resolve_ok(COMPLETE, &overrides);
        assert_eq!(cfg.dependencies.as_deref(), Some("nodejs"));
    }

    /// `0ad` is a legal package name that `useradd` refuses: derive the account name and warn.
    #[test]
    fn account_names_are_derived_from_package_names_that_shadow_utils_reject() {
        let json = r#"{
            "name": "0ad", "version": "1.0.0", "description": "d", "author": "a",
            "nativepkg": { "init": "none" }
        }"#;
        let (cfg, warnings) = resolve_ok(json, &Overrides::default());
        assert_eq!(cfg.package_name.as_str(), "0ad");
        assert_eq!(cfg.user.as_str(), "_0ad");
        assert_eq!(cfg.group.as_str(), "_0ad");
        assert!(warnings.contains(&Warning::UnixNameDerived {
            kind: "user",
            source: "0ad".into(),
            derived: "_0ad".into(),
        }));
    }

    /// An explicit account name is validated, never sanitised; this would otherwise reach
    /// `useradd`.
    #[test]
    fn explicit_account_names_with_metacharacters_are_refused() {
        let overrides = Overrides {
            user: Some("bad user; rm -rf /".into()),
            ..Overrides::default()
        };
        let err = resolve(&manifest(COMPLETE), &overrides)
            .expect_err("a shell-metacharacter user name must not resolve");
        assert!(matches!(err, Error::InvalidUnixName { .. }), "{err:?}");
    }

    #[test]
    fn embedded_hyphen_in_a_prerelease_warns() {
        let overrides = Overrides {
            version: Some("2.0.0-experimental-build".into()),
            ..Overrides::default()
        };
        let (cfg, warnings) = resolve_ok(COMPLETE, &overrides);
        assert_eq!(cfg.version.deb(), "2.0.0~experimental.build");
        assert!(warnings.contains(&Warning::VersionHyphensRewritten {
            original: "2.0.0-experimental-build".into(),
        }));

        // The warning must not quote a spelling: the mapped form differs per format.
        let text = warnings
            .iter()
            .find(|w| matches!(w, Warning::VersionHyphensRewritten { .. }))
            .expect("the warning")
            .to_string();
        assert!(
            !text.contains("2.0.0~experimental.build"),
            "the warning quotes one format's spelling: {text}"
        );
    }

    #[test]
    fn object_author_resolves_to_a_formatted_maintainer() {
        let json = r#"{
            "name": "app", "version": "1.0.0", "description": "d",
            "author": { "name": "Ivan", "email": "i@example.com" },
            "nativepkg": { "init": "none" }
        }"#;
        let (cfg, _) = resolve_ok(json, &Overrides::default());
        assert_eq!(cfg.maintainer, "Ivan <i@example.com>");
    }
}
