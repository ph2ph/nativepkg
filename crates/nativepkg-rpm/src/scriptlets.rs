//! Composing RPM scriptlets from named snippets.
//!
//! A separate implementation from the Debian backend's `scripts` module on purpose: RPM's `$1`
//! is a count of this package after the operation (1 install, 2 upgrade, 0 erase) where dpkg's
//! is an action word, there is no `purge` distinct from the final erase, and the unit lifecycle
//! goes through `systemctl preset` / `try-restart`. One snippet set would need a conditional in
//! every snippet, evaluated on the target.
//!
//! The systemd snippets are `%systemd_post`, `%systemd_preun` and `%systemd_postun_with_restart`
//! written out: nothing expands macros in a package `rpmbuild` did not build. Where a snippet
//! adds to the macro, the snippet says so.

use core::fmt::Write as _;

use nativepkg_core::npm::{InitSystem, InstallStrategy};
use nativepkg_core::template::{Variables, render};
use nativepkg_core::{Error, Result};

/// POSIX `sh`, RPM's default scriptlet interpreter.
const SHEBANG: &str = "#!/bin/sh";

/// RPM's four scriptlet slots; `preun` and `postun` run before and after erase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scriptlet {
    Pre,
    Post,
    Preun,
    Postun,
}

impl Scriptlet {
    /// RPM's own name for the slot, without the leading `%`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Pre => "pre",
            Self::Post => "post",
            Self::Preun => "preun",
            Self::Postun => "postun",
        }
    }

    #[must_use]
    pub fn all() -> [Self; 4] {
        [Self::Pre, Self::Post, Self::Preun, Self::Postun]
    }
}

const SNIPPETS: &[(&str, &str)] = &[
    ("pre-account", include_str!("../snippets/pre-account")),
    ("post-logdir", include_str!("../snippets/post-logdir")),
    (
        "post-npm-install",
        include_str!("../snippets/post-npm-install"),
    ),
    ("post-systemd", include_str!("../snippets/post-systemd")),
    ("preun-systemd", include_str!("../snippets/preun-systemd")),
    ("postun-systemd", include_str!("../snippets/postun-systemd")),
    ("postun-purge", include_str!("../snippets/postun-purge")),
    ("post-sysv", include_str!("../snippets/post-sysv")),
    ("preun-sysv", include_str!("../snippets/preun-sysv")),
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

/// Which snippets a scriptlet is composed from. Selected at build time, so a package built for
/// systemd carries no sysv logic.
#[must_use]
pub fn snippets_for(
    scriptlet: Scriptlet,
    init: InitSystem,
    strategy: InstallStrategy,
) -> Vec<&'static str> {
    if init == InitSystem::None {
        return match (scriptlet, strategy) {
            (Scriptlet::Post, InstallStrategy::NpmInstall) => vec!["post-npm-install"],
            _ => Vec::new(),
        };
    }

    // Upstart is deliberately absent: RHEL 6 is long past end of life, and a job file no RPM
    // distribution reads would be worse than nothing. The caller reports it instead.
    let uses = |wanted: InitSystem| init == InitSystem::Auto || init == wanted;
    let mut chosen = Vec::new();

    match scriptlet {
        Scriptlet::Pre => chosen.push("pre-account"),
        Scriptlet::Post => {
            if strategy == InstallStrategy::NpmInstall {
                chosen.push("post-npm-install");
            }
            chosen.push("post-logdir");
            if uses_preset(init) {
                chosen.push("post-systemd");
            }
            if uses(InitSystem::Sysv) {
                chosen.push("post-sysv");
            }
        }
        Scriptlet::Preun => {
            if uses(InitSystem::Systemd) {
                chosen.push("preun-systemd");
            }
            if uses(InitSystem::Sysv) {
                chosen.push("preun-sysv");
            }
        }
        Scriptlet::Postun => {
            if uses(InitSystem::Systemd) {
                chosen.push("postun-systemd");
            }
            chosen.push("postun-purge");
        }
    }
    chosen
}

/// Whether the scriptlets will `systemctl preset` the unit, and so whether the package must ship
/// the preset policy. One predicate for both, so snippet selection and the CLI cannot drift.
#[must_use]
pub fn uses_preset(init: InitSystem) -> bool {
    matches!(init, InitSystem::Auto | InitSystem::Systemd)
}

/// The packages this configuration's scriptlets call.
///
/// Declared beside the snippet selection so the two cannot disagree; on Debian a scriptlet
/// calling a program the package did not require was found by a container run, not by reading.
#[must_use]
pub fn required_packages(init: InitSystem, strategy: InstallStrategy) -> Vec<&'static str> {
    let uses = |name: &str| {
        Scriptlet::all()
            .iter()
            .any(|slot| snippets_for(*slot, init, strategy).contains(&name))
    };

    let mut required = Vec::new();
    if uses("pre-account") || uses("postun-purge") {
        // `useradd`, `groupadd`, `userdel` and `groupdel`.
        required.push("shadow-utils");
    }
    if uses("post-systemd") || uses("preun-systemd") || uses("postun-systemd") {
        required.push("systemd");
    }
    required
}

/// Whether `%pre` itself calls this dependency, which makes it `Requires(pre)`.
#[must_use]
pub fn runs_before_install(name: &str) -> bool {
    name == "shadow-utils"
}

/// Composes one scriptlet, returning the text and any rendering warnings.
///
/// # Errors
///
/// [`Error::Template`] when a snippet references a variable the vocabulary does not supply.
pub fn compose(
    scriptlet: Scriptlet,
    init: InitSystem,
    strategy: InstallStrategy,
    variables: &Variables,
) -> Result<String> {
    let chosen = snippets_for(scriptlet, init, strategy);

    let generator = variables.resolve("generator_version").unwrap_or("unknown");

    let mut out = String::with_capacity(1024);
    out.push_str(SHEBANG);
    out.push('\n');
    let _ = writeln!(
        out,
        "# This file was autogenerated by nativepkg {generator}"
    );

    // No `set -e`: RPM aborts the whole transaction when a scriptlet exits non-zero, which is
    // also why every call in the snippets ends `|| :`.
    for name in chosen {
        let source = snippet(name)?;
        let rendered = render(name, source, variables)?;
        out.push('\n');
        out.push_str(&rendered);
    }

    out.push_str("\nexit 0\n");
    Ok(out)
}
