//! The command line surface.
//!
//! Every option the bash implementation accepted is here with the same spelling, so existing
//! build scripts keep working. That surface is 36 branches, not the 26 a `grep '^\s*--'` finds:
//! eleven are written `-a | --architecture)`. See `openspec/cli-surface.md`.
//!
//! Options not carried over as-is are handled explicitly: deprecated ones warn and name the
//! replacement, ones that can no longer be honoured are refused with a reason, and ones the
//! bash implementation had broken are repaired.

use std::path::PathBuf;

use clap::{ArgAction, Parser};

use nativepkg_core::npm::{InitSystem, InstallStrategy};

use crate::format::Format;

/// Init systems the tool can integrate with.
///
/// A `ValueEnum` rather than a free string: an earlier version matched by hand and returned
/// `None` for anything unknown, so `--init Systemd` (wrong case) was dropped in silence and
/// the config's value won.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum InitChoice {
    /// Integrate with whatever the target has.
    Auto,
    Systemd,
    Upstart,
    Sysv,
    /// No service integration at all.
    None,
}

impl From<InitChoice> for InitSystem {
    fn from(choice: InitChoice) -> Self {
        match choice {
            InitChoice::Auto => Self::Auto,
            InitChoice::Systemd => Self::Systemd,
            InitChoice::Upstart => Self::Upstart,
            InitChoice::Sysv => Self::Sysv,
            InitChoice::None => Self::None,
        }
    }
}

/// How dependencies get into the package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum StrategyChoice {
    /// Decide from the project's shape.
    Auto,
    /// Copy `node_modules` as it is.
    Copy,
    /// Run `npm install` when the package is installed.
    #[value(name = "npm-install")]
    NpmInstall,
}

impl From<StrategyChoice> for InstallStrategy {
    fn from(choice: StrategyChoice) -> Self {
        match choice {
            StrategyChoice::Auto => Self::Auto,
            StrategyChoice::Copy => Self::Copy,
            StrategyChoice::NpmInstall => Self::NpmInstall,
        }
    }
}

/// Build native Linux packages from any project.
#[derive(Debug, Parser)]
// A flat list of flags; splitting it into sub-structs to satisfy the lint would hide the
// shape of the interface.
#[allow(clippy::struct_excessive_bools)]
#[command(
    name = "nativepkg",
    about = "Build .deb, .rpm and .pkg.tar.zst packages from any project",
    long_about = None,
    after_help = "Environment:\n  NATIVEPKG_BINARY_PATH  when installed from npm, run this binary instead of the \
                  bundled one",
    // `-v, --version` is the *package* version, as in the bash tool; the binary's own version
    // is `--tool-version`, so clap's automatic flag must not claim the name.
    disable_version_flag = true,
)]
pub struct Cli {
    // -- package identity ------------------------------------------------------------------
    /// Package name; defaults to `package_name` in `.nativepkg`.
    #[arg(short = 'n', long = "pkg-name")]
    pub package_name: Option<String>,

    /// Package version; defaults to `version` in `.nativepkg`.
    #[arg(short = 'v', long)]
    pub version: Option<String>,

    /// Print this tool's own version and exit.
    #[arg(long, action = ArgAction::SetTrue)]
    pub tool_version: bool,

    /// Version epoch, for when an upstream version scheme goes backwards.
    #[arg(long)]
    pub epoch: Option<u32>,

    /// One-line description; defaults to `description` in `.nativepkg`.
    #[arg(short = 'd', long)]
    pub description: Option<String>,

    /// Maintainer, as `Name <email>`.
    #[arg(short = 'm', long)]
    pub maintainer: Option<String>,

    /// Target architecture.
    #[arg(short = 'a', long)]
    pub architecture: Option<String>,

    // The bash tool has warned about this for years; keep accepting it, keep warning.
    /// Deprecated spelling of `--architecture`.
    #[arg(long = "arch", value_name = "ARCH")]
    pub arch_deprecated: Option<String>,

    // -- payload ---------------------------------------------------------------------------
    /// Name of the executable placed on `PATH`.
    #[arg(short = 'e', long = "exec-name")]
    pub executable_name: Option<String>,

    // Repaired: the bash tool parsed this and discarded it (misspelled assignment).
    /// Directory the application is installed into.
    #[arg(long)]
    pub install_dir: Option<String>,

    /// How dependencies get into the package.
    #[arg(long, value_enum)]
    pub install_strategy: Option<StrategyChoice>,

    /// Override the install-at-unpack command (default: plain `npm install`).
    #[arg(long)]
    pub install_command: Option<String>,

    /// Override the binary the install command's guard checks for (default: derived from it).
    #[arg(long)]
    pub install_binary: Option<String>,

    /// Extra files to include, as a whitespace-separated list.
    #[arg(long)]
    pub extra_files: Option<String>,

    /// A dpkg `triggers` control file to ship verbatim (Debian only).
    #[arg(long, value_name = "PATH")]
    pub triggers_file: Option<String>,

    /// The file the service runs, relative to the project root (e.g. `index.js`).
    #[arg(long = "daemon", value_name = "FILE")]
    pub daemon_entrypoint: Option<String>,

    /// The file the command-line wrapper runs; defaults to the daemon entry point.
    #[arg(long = "cli", value_name = "FILE")]
    pub cli_entrypoint: Option<String>,

    // -- service ---------------------------------------------------------------------------
    /// Init system to integrate with.
    #[arg(short = 'i', long, value_enum)]
    pub init: Option<InitChoice>,

    /// User the service runs as.
    #[arg(short = 'u', long)]
    pub user: Option<String>,

    /// Group the service runs as.
    #[arg(short = 'g', long)]
    pub group: Option<String>,

    // -- dependencies ----------------------------------------------------------------------
    /// Package dependencies, as a comma-separated list.
    #[arg(long)]
    pub deps: Option<String>,

    /// Add `nodejs` as a dependency; for a Node.js application.
    #[arg(long, action = ArgAction::SetTrue)]
    pub nodejs: bool,

    // -- output ----------------------------------------------------------------------------
    /// Formats to build.
    #[arg(long, value_delimiter = ',', default_value = "deb")]
    pub format: Vec<Format>,

    /// Directory the packages are written to.
    #[arg(long, default_value = ".")]
    pub output_dir: PathBuf,

    /// Base name of the output file.
    #[arg(short = 'o', long, visible_alias = "output-deb-name")]
    pub output_name: Option<String>,

    // -- behaviour -------------------------------------------------------------------------
    /// Resolve and plan, but write nothing.
    #[arg(long, action = ArgAction::SetTrue)]
    pub dry_run: bool,

    /// Emit machine-readable output.
    #[arg(long, action = ArgAction::SetTrue)]
    pub json: bool,

    /// Print more about what is happening.
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "quiet")]
    pub verbose: bool,

    /// Print only errors.
    #[arg(long, action = ArgAction::SetTrue)]
    pub quiet: bool,

    // -- introspection ---------------------------------------------------------------------
    /// List the config keys accepted in `.nativepkg`.
    #[arg(long, action = ArgAction::SetTrue)]
    pub list_json_overrides: bool,

    /// List the available templates.
    #[arg(long = "list-tmps", action = ArgAction::SetTrue)]
    pub list_templates: bool,

    /// List the variables templates may use.
    #[arg(long = "list-tmp-vars", action = ArgAction::SetTrue)]
    pub list_template_variables: bool,

    /// Print one template by name.
    #[arg(long = "cat-tmp", value_name = "NAME")]
    pub cat_template: Option<String>,

    /// Print this tool's README.
    #[arg(long, action = ArgAction::SetTrue)]
    pub show_readme: bool,

    /// Print this tool's changelog.
    #[arg(long, action = ArgAction::SetTrue)]
    pub show_changelog: bool,

    // -- template overrides (the five templates that survive; the rest are refused below) ----
    /// Path to a replacement for the `/usr/bin` wrapper template.
    #[arg(long = "tmp-exec", value_name = "PATH")]
    pub template_executable: Option<PathBuf>,

    /// Path to a replacement for the systemd unit template.
    #[arg(long = "tmp-systemd-service", value_name = "PATH")]
    pub template_systemd_service: Option<PathBuf>,

    /// Path to a replacement for the System V init script template.
    #[arg(long = "tmp-sysv-init", value_name = "PATH")]
    pub template_sysv_init: Option<PathBuf>,

    /// Path to a replacement for the Upstart job template.
    #[arg(long = "tmp-upstart-cnf", value_name = "PATH")]
    pub template_upstart_conf: Option<PathBuf>,

    /// Path to a replacement for the `/etc/default` variables template.
    #[arg(long = "tmp-default-variables", value_name = "PATH")]
    pub template_default: Option<PathBuf>,

    // -- options that can no longer be honoured (hidden; `removed_options` says why) --------
    #[arg(long, action = ArgAction::SetTrue, hide = true)]
    pub no_delete_temp: bool,

    #[arg(long, action = ArgAction::SetTrue, hide = true)]
    pub no_md5sums: bool,

    #[arg(long, action = ArgAction::SetTrue, hide = true)]
    pub no_rebuild: bool,

    #[arg(long, value_name = "PATH", hide = true)]
    pub template_control: Option<String>,

    #[arg(long, value_name = "PATH", hide = true)]
    pub template_preinst: Option<String>,

    #[arg(long, value_name = "PATH", hide = true)]
    pub template_postinst: Option<String>,

    #[arg(long, value_name = "PATH", hide = true)]
    pub template_prerm: Option<String>,

    #[arg(long, value_name = "PATH", hide = true)]
    pub template_postrm: Option<String>,

    // -- inputs ------------------------------------------------------------------------------
    /// Files and directories to package.
    #[arg(trailing_var_arg = true)]
    pub inputs: Vec<PathBuf>,
}

/// An option that is gone, and why.
pub struct Removed {
    pub option: &'static str,
    pub reason: &'static str,
}

impl Cli {
    /// Every removed option the user passed. Refused, not ignored: a build script must not
    /// keep working while quietly losing what it asked for.
    #[must_use]
    pub fn removed_options(&self) -> Vec<Removed> {
        let mut found = Vec::new();
        let mut note = |flag, reason| {
            found.push(Removed {
                option: flag,
                reason,
            });
        };

        if self.no_delete_temp {
            note(
                "--no-delete-temp",
                "there is no staging directory to keep: files are streamed into the archive \
                 from the project tree",
            );
        }
        if self.no_md5sums {
            note(
                "--no-md5sums",
                "Debian policy requires a `md5sums` control file, and omitting it makes the \
                 package fail `lintian`",
            );
        }
        if self.no_rebuild {
            note(
                "--no-rebuild",
                "this tool never invokes a build step, so there is nothing to skip",
            );
        }
        if self.template_control.is_some() {
            note(
                "--template-control",
                "the control file is written from the plan's data rather than rendered from a \
                 template, which is what makes a multi-line description safe",
            );
        }
        for (flag, present) in [
            ("--template-preinst", self.template_preinst.is_some()),
            ("--template-postinst", self.template_postinst.is_some()),
            ("--template-prerm", self.template_prerm.is_some()),
            ("--template-postrm", self.template_postrm.is_some()),
        ] {
            if present {
                note(
                    flag,
                    "maintainer scripts are composed from named snippets selected at build \
                     time, so a whole-file override is no longer the unit of replacement",
                );
            }
        }
        found
    }

    /// The template overrides the user supplied, by built-in template name.
    #[must_use]
    pub fn template_overrides(&self) -> Vec<(&'static str, &std::path::Path)> {
        [
            ("executable", self.template_executable.as_deref()),
            ("systemd.service", self.template_systemd_service.as_deref()),
            ("sysv-init", self.template_sysv_init.as_deref()),
            ("upstart.conf", self.template_upstart_conf.as_deref()),
            ("default", self.template_default.as_deref()),
        ]
        .into_iter()
        .filter_map(|(name, path)| path.map(|p| (name, p)))
        .collect()
    }

    #[must_use]
    pub fn is_introspection(&self) -> bool {
        self.list_json_overrides
            || self.list_templates
            || self.list_template_variables
            || self.cat_template.is_some()
            || self.show_readme
            || self.show_changelog
    }
}
