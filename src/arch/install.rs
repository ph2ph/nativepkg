//! Composing the Arch `.INSTALL` scriptlet.
//!
//! One file of named shell functions — `post_install`, `post_upgrade`, `pre_remove`,
//! `post_remove` — and `pacman` calls the one matching the operation; the function name is the
//! dispatch, where Debian branches on an action word in `$1` and RPM on a package count.
//!
//! Arch is systemd-only, so there are no sysv or Upstart snippets; [`unsupported_init`] reports
//! such a configuration rather than producing a package whose service never starts.

use core::fmt::Write as _;

use crate::core::npm::{InitSystem, InstallStrategy};
use crate::core::template::{Variables, render};
use crate::core::{Error, Result};

/// The functions `pacman` may call, in emission order. Both remove hooks are needed: the unit is
/// stopped while its files still exist, the account removed after they are gone.
const FUNCTIONS: &[&str] = &["post_install", "post_upgrade", "pre_remove", "post_remove"];

const SNIPPETS: &[(&str, &str)] = &[
    ("account", include_str!("../../snippets/arch/account")),
    ("logdir", include_str!("../../snippets/arch/logdir")),
    (
        "npm-install",
        include_str!("../../snippets/arch/npm-install"),
    ),
    (
        "systemd-reload",
        include_str!("../../snippets/arch/systemd-reload"),
    ),
    (
        "systemd-enable",
        include_str!("../../snippets/arch/systemd-enable"),
    ),
    (
        "systemd-restart",
        include_str!("../../snippets/arch/systemd-restart"),
    ),
    (
        "systemd-start",
        include_str!("../../snippets/arch/systemd-start"),
    ),
    (
        "systemd-stop",
        include_str!("../../snippets/arch/systemd-stop"),
    ),
    ("purge", include_str!("../../snippets/arch/purge")),
];

#[must_use]
pub fn snippet_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = SNIPPETS.iter().map(|(name, _)| *name).collect();
    names.sort_unstable();
    names
}

/// The source of one snippet; an unknown name is an [`Error::Template`] listing the valid ones.
pub fn snippet(name: &str) -> Result<&'static str> {
    SNIPPETS
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, source)| *source)
        .ok_or_else(|| Error::Template {
            template: name.to_owned(),
            reason: format!(
                "unknown snippet; valid names: {}",
                snippet_names().join(", ")
            ),
        })
}

/// The init system this backend cannot serve, if the configuration names one. Reported rather
/// than ignored: a package that installs but never starts its service is worse than an error.
#[must_use]
pub fn unsupported_init(init: InitSystem) -> Option<&'static str> {
    match init {
        InitSystem::Sysv => Some("sysv"),
        InitSystem::Upstart => Some("upstart"),
        InitSystem::Systemd | InitSystem::Auto | InitSystem::None => None,
    }
}

/// Whether `post_install` will `systemctl preset` the unit, and so whether the package must ship
/// the preset policy. One predicate for both, so snippet selection and the CLI cannot drift.
#[must_use]
pub fn uses_preset(init: InitSystem) -> bool {
    !matches!(init, InitSystem::None) && unsupported_init(init).is_none()
}

/// Which snippets one function is composed from.
#[must_use]
fn snippets_for(function: &str, init: InitSystem, strategy: InstallStrategy) -> Vec<&'static str> {
    let service = uses_preset(init);

    match function {
        "post_install" => {
            let mut chosen = Vec::new();
            if strategy == InstallStrategy::NpmInstall {
                chosen.push("npm-install");
            }
            if service {
                chosen.push("account");
                chosen.push("logdir");
                chosen.push("systemd-reload");
                chosen.push("systemd-enable");
                chosen.push("systemd-start");
            }
            chosen
        }
        // No `preset` on upgrade: the administrator may have disabled the unit since.
        "post_upgrade" if service => vec!["systemd-reload", "systemd-restart"],
        "pre_remove" if service => vec!["systemd-stop"],
        "post_remove" if service => vec!["systemd-reload", "purge"],
        _ => Vec::new(),
    }
}

/// Composes the whole `.INSTALL` file, or `None` when no function would have a body.
///
/// # Errors
///
/// [`Error::Template`] when a snippet references a variable the vocabulary does not supply.
pub fn compose(
    init: InitSystem,
    strategy: InstallStrategy,
    variables: &Variables,
) -> Result<Option<String>> {
    let bodies: Vec<(&str, Vec<&'static str>)> = FUNCTIONS
        .iter()
        .map(|function| (*function, snippets_for(function, init, strategy)))
        .filter(|(_, chosen)| !chosen.is_empty())
        .collect();

    if bodies.is_empty() {
        // No functions, no file: an empty `.INSTALL` would describe behaviour the package lacks.
        return Ok(None);
    }

    let generator = variables.resolve("generator_version").unwrap_or("unknown");

    let mut out = String::with_capacity(1024);
    let _ = writeln!(
        out,
        "# This file was autogenerated by nativepkg {generator}"
    );

    for (function, chosen) in bodies {
        let _ = writeln!(out, "\n{function}() {{");
        for name in chosen {
            let source = snippet(name)?;
            out.push_str(&render(name, source, variables)?);
        }
        out.push_str("}\n");
    }

    Ok(Some(out))
}
