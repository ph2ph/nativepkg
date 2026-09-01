//! Composing Debian maintainer scripts from named snippets.
//!
//! Everything here is dpkg-shaped — the four script names, `deb-systemd-helper`, `adduser`,
//! `update-rc.d`, dispatch on `$1`/`$2` — so it lives in this backend, not the core. RPM and
//! Arch compose their own.
//!
//! As debhelper does, snippets are selected at build time rather than branched on at install
//! time, so what ships is exactly what was tested. They follow the real `dh_installsystemd`
//! fragments: guard on `/run/systemd/system`, not on `systemctl` existing; `deb-systemd-helper
//! enable`, which respects an administrator's disable; `deb-systemd-invoke`, which honours
//! `policy-rc.d`; `daemon-reload` before starting; dispatch on `$1`; `|| true` on every
//! service call.

use core::fmt::Write as _;

use nativepkg_core::npm::{InitSystem, InstallStrategy};
use nativepkg_core::template::{Variables, render};
// Snippet lookup and rendering are template failures, so they use the core's error type.
use nativepkg_core::{Error, Result};

/// POSIX `sh`, like Debian's own maintainer scripts and every debhelper fragment.
const SHEBANG: &str = "#!/bin/sh";

/// The four scripts dpkg may invoke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Script {
    Preinst,
    Postinst,
    Prerm,
    Postrm,
}

impl Script {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Preinst => "preinst",
            Self::Postinst => "postinst",
            Self::Prerm => "prerm",
            Self::Postrm => "postrm",
        }
    }

    #[must_use]
    pub fn all() -> [Self; 4] {
        [Self::Preinst, Self::Postinst, Self::Prerm, Self::Postrm]
    }
}

const SNIPPETS: &[(&str, &str)] = &[
    (
        "postinst-account",
        include_str!("../snippets/postinst-account"),
    ),
    (
        "postinst-logdir",
        include_str!("../snippets/postinst-logdir"),
    ),
    (
        "postinst-npm-install",
        include_str!("../snippets/postinst-npm-install"),
    ),
    (
        "postinst-systemd-enable",
        include_str!("../snippets/postinst-systemd-enable"),
    ),
    (
        "postinst-systemd-restart",
        include_str!("../snippets/postinst-systemd-restart"),
    ),
    ("postinst-sysv", include_str!("../snippets/postinst-sysv")),
    (
        "postinst-upstart",
        include_str!("../snippets/postinst-upstart"),
    ),
    ("postrm-purge", include_str!("../snippets/postrm-purge")),
    ("postrm-systemd", include_str!("../snippets/postrm-systemd")),
    (
        "postrm-systemd-reload",
        include_str!("../snippets/postrm-systemd-reload"),
    ),
    ("postrm-sysv", include_str!("../snippets/postrm-sysv")),
    (
        "prerm-systemd-restart",
        include_str!("../snippets/prerm-systemd-restart"),
    ),
    ("prerm-sysv", include_str!("../snippets/prerm-sysv")),
    ("prerm-upstart", include_str!("../snippets/prerm-upstart")),
];

#[must_use]
pub fn snippet_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = SNIPPETS.iter().map(|(name, _)| *name).collect();
    names.sort_unstable();
    names
}

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

/// Which snippets a script is composed from. Selection is at build time, not `if`s on the
/// target, so a package built for systemd contains no Upstart logic at all.
#[must_use]
pub fn snippets_for(
    script: Script,
    init: InitSystem,
    strategy: InstallStrategy,
) -> Vec<&'static str> {
    if init == InitSystem::None {
        // No service: no unit, account or log directory; only install-time dependencies.
        return match (script, strategy) {
            (Script::Postinst, InstallStrategy::NpmInstall) => vec!["postinst-npm-install"],
            _ => Vec::new(),
        };
    }

    // `auto` fans out to systemd and System V, not to Upstart: see the note in
    // `build::service_files`. Upstart is composed only when named.
    let uses = |wanted: InitSystem| init == InitSystem::Auto || init == wanted;
    let mut chosen = Vec::new();

    match script {
        Script::Preinst => {}
        Script::Postinst => {
            if strategy == InstallStrategy::NpmInstall {
                chosen.push("postinst-npm-install");
            }
            chosen.push("postinst-account");
            chosen.push("postinst-logdir");
            if uses(InitSystem::Systemd) {
                chosen.push("postinst-systemd-enable");
                chosen.push("postinst-systemd-restart");
            }
            if uses(InitSystem::Sysv) {
                chosen.push("postinst-sysv");
            }
            if init == InitSystem::Upstart {
                chosen.push("postinst-upstart");
            }
        }
        Script::Prerm => {
            if uses(InitSystem::Systemd) {
                chosen.push("prerm-systemd-restart");
            }
            if uses(InitSystem::Sysv) {
                chosen.push("prerm-sysv");
            }
            if init == InitSystem::Upstart {
                chosen.push("prerm-upstart");
            }
        }
        Script::Postrm => {
            if uses(InitSystem::Systemd) {
                chosen.push("postrm-systemd");
            }
            if uses(InitSystem::Sysv) {
                chosen.push("postrm-sysv");
            }
            chosen.push("postrm-purge");
            if uses(InitSystem::Systemd) {
                // After the unit file is gone, so the reload observes its absence.
                chosen.push("postrm-systemd-reload");
            }
        }
    }
    chosen
}

/// Packages the composed scripts need at run time. Declared where the snippets are chosen, so
/// the two cannot disagree. Not hypothetical: `adduser` left Debian's Essential set, so on a
/// `-slim` image `addgroup` is absent and `postinst-account` under `set -e` made
/// `dpkg --install` exit 127. A container run found it on the first attempt.
#[must_use]
pub fn required_packages(init: InitSystem, strategy: InstallStrategy) -> Vec<&'static str> {
    let mut required = Vec::new();

    let uses = |name: &str| {
        Script::all()
            .iter()
            .any(|script| snippets_for(*script, init, strategy).contains(&name))
    };

    if uses("postinst-account") || uses("postrm-purge") {
        // `adduser`, `addgroup` and `deluser` all ship in this package (Policy 9.2.2).
        required.push("adduser");
    }
    if uses("postinst-systemd-enable")
        || uses("postinst-systemd-restart")
        || uses("prerm-systemd-restart")
        || uses("postrm-systemd")
    {
        // `deb-systemd-helper` and `deb-systemd-invoke`; debhelper adds this via
        // `${misc:Depends}`. Versioned because `init-system-helpers` is Essential and Policy
        // 3.5 allows depending on an Essential package only with a version; 1.54 is what
        // debhelper names.
        required.push("init-system-helpers (>= 1.54)");
    }

    required
}

/// Composes one maintainer script. Fails only when a snippet and the vocabulary have drifted apart.
pub fn compose(
    script: Script,
    init: InitSystem,
    strategy: InstallStrategy,
    variables: &Variables,
) -> Result<String> {
    let chosen = snippets_for(script, init, strategy);

    let generator = variables.resolve("generator_version").unwrap_or("unknown");

    let mut out = String::with_capacity(1024);
    out.push_str(SHEBANG);
    out.push('\n');
    let _ = writeln!(
        out,
        "# This file was autogenerated by nativepkg {generator}"
    );
    out.push_str("set -e\n");

    for name in chosen {
        let source = snippet(name)?;
        let rendered = render(name, source, variables)?;
        out.push('\n');
        out.push_str(&rendered);
    }

    // dpkg may pass an action this script does not handle; exiting 0 is the documented behaviour.
    out.push_str("\nexit 0\n");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Variables {
        Variables::new()
            .with("package_name", "probe-app")
            .with("package_description", "a probe")
            .with("package_maintainer", "A <a@example.com>")
            .with("install_dir", "/usr/lib")
            .with("user", "probe-app")
            .with("group", "probe-app")
            .with("generator_version", "0.1.0")
            .with("install_binary", "npm")
            .with(
                "install_command",
                "npm install --omit=dev --ignore-scripts --no-audit --no-fund",
            )
            .with("daemon_entrypoint", "app.js")
    }

    fn composed_raw(script: Script, init: InitSystem, strategy: InstallStrategy) -> String {
        compose(script, init, strategy, &vars())
            .unwrap_or_else(|e| panic!("{script:?} for {init:?} should compose: {e}"))
    }

    /// Comment lines stripped: a snippet comment explaining why it avoids `systemctl enable`
    /// once tripped the assertion forbidding it. Only shape tests use [`composed_raw`].
    fn composed(script: Script, init: InitSystem, strategy: InstallStrategy) -> String {
        composed_raw(script, init, strategy)
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn every_snippet_renders_with_the_vocabulary() {
        for name in snippet_names() {
            let source = snippet(name).expect("snippet should exist");
            render(name, source, &vars())
                .unwrap_or_else(|e| panic!("snippet `{name}` failed to render: {e}"));
        }
    }

    #[test]
    fn an_unknown_snippet_lists_the_valid_names() {
        let err = snippet("nope").expect_err("unknown").to_string();
        assert!(err.contains("postinst-account"), "{err}");
    }

    #[test]
    fn a_systemd_package_contains_no_other_init_logic() {
        for script in Script::all() {
            let text = composed(script, InitSystem::Systemd, InstallStrategy::Copy);
            assert!(
                !text.contains("initctl"),
                "{script:?} contains Upstart logic"
            );
            assert!(
                !text.contains("update-rc.d") && !text.contains("invoke-rc.d"),
                "{script:?} contains SysV logic"
            );
        }
    }

    #[test]
    fn a_sysv_package_contains_no_systemd_logic() {
        for script in Script::all() {
            let text = composed(script, InitSystem::Sysv, InstallStrategy::Copy);
            assert!(
                !text.contains("deb-systemd"),
                "{script:?} contains systemd logic"
            );
        }
    }

    /// Over the whole set, not one file: `prerm-upstart` was the snippet missed when the guard
    /// was added to the System V three.
    #[test]
    fn every_legacy_init_snippet_defers_to_systemd() {
        let legacy: Vec<&str> = snippet_names()
            .into_iter()
            .filter(|name| name.contains("sysv") || name.contains("upstart"))
            .collect();
        assert!(
            legacy.len() >= 5,
            "expected the SysV and Upstart snippets, found {legacy:?}"
        );

        for name in legacy {
            let source = snippet(name).expect("snippet should exist");
            // `postrm-sysv` only unregisters an init script; there is nothing to defer.
            if name == "postrm-sysv" {
                continue;
            }
            assert!(
                source.contains("[ -d /run/systemd/system ]"),
                "`{name}` acts on a legacy init without checking whether systemd is running; \
                 under `auto` that can supervise the same process twice"
            );
        }
    }

    /// The block extractor relies on `if` and `fi` on their own lines; a one-line
    /// `if ...; fi` would never close its depth count.
    #[test]
    fn no_snippet_uses_a_single_line_if() {
        for name in snippet_names() {
            let source = snippet(name).expect("snippet should exist");
            for (number, line) in source.lines().enumerate() {
                let trimmed = line.trim();
                assert!(
                    !(trimmed.starts_with("if ") && trimmed.ends_with("fi")),
                    "`{name}` line {} is a one-line `if`, which the block extractor three \
                     behavioural tests rely on cannot close: {trimmed}",
                    number + 1
                );
            }
        }
    }

    /// Not Upstart: Debian removed it in stretch, and composing it under `auto` shipped a dead
    /// job file in every package.
    #[test]
    fn auto_covers_the_init_systems_a_current_target_has() {
        let postinst = composed(Script::Postinst, InitSystem::Auto, InstallStrategy::Copy);
        assert!(postinst.contains("deb-systemd-helper"));
        assert!(postinst.contains("update-rc.d"));
        assert!(
            !postinst.contains("initctl"),
            "auto must not compose Upstart:\n{postinst}"
        );

        let explicit = composed(Script::Postinst, InitSystem::Upstart, InstallStrategy::Copy);
        assert!(
            explicit.contains("initctl"),
            "named Upstart is still composed"
        );
    }

    #[test]
    fn init_none_produces_no_service_account_or_unit_references() {
        for script in Script::all() {
            let text = composed(script, InitSystem::None, InstallStrategy::Copy);
            for forbidden in [
                "deb-systemd",
                "initctl",
                "update-rc.d",
                "adduser",
                "addgroup",
                "/var/log/",
            ] {
                assert!(
                    !text.contains(forbidden),
                    "{script:?} with init=none mentions `{forbidden}`:\n{text}"
                );
            }
        }
    }

    #[test]
    fn every_script_dispatches_on_its_argument() {
        for script in [Script::Postinst, Script::Prerm, Script::Postrm] {
            let text = composed(script, InitSystem::Systemd, InstallStrategy::Copy);
            assert!(
                text.contains("\"$1\""),
                "{script:?} must act on the action dpkg reports:\n{text}"
            );
        }
    }

    /// Bodies of the `if` blocks whose condition mentions `needle`. A line appearing
    /// *somewhere* in the script says nothing about which guard it sits under.
    fn guarded_block(text: &str, needle: &str) -> String {
        let mut out = String::new();
        let mut depth = 0usize;
        let mut inside = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if !inside {
                if trimmed.starts_with("if ") && trimmed.contains(needle) {
                    inside = true;
                    depth = 1;
                }
                continue;
            }
            if trimmed.starts_with("if ") {
                depth += 1;
            }
            if trimmed == "fi" {
                depth -= 1;
                if depth == 0 {
                    // Several blocks may share a guard, one per snippet; keep going.
                    inside = false;
                    continue;
                }
            }
            out.push_str(line);
            out.push('\n');
        }
        out
    }

    /// Guards against the extractor returning everything, or only the first match.
    #[test]
    fn the_block_extractor_isolates_every_matching_guard() {
        let script = concat!(
            "if [ \"$1\" = \"a\" ]; then\n\tfirst_a\nfi\n",
            "if [ \"$1\" = \"b\" ]; then\n\tonly_in_b\nfi\n",
            "if [ \"$1\" = \"a\" ]; then\n\tsecond_a\nfi\n"
        );
        let a = guarded_block(script, "\"$1\" = \"a\"");
        assert!(a.contains("first_a"), "{a}");
        assert!(
            a.contains("second_a"),
            "a second block with the same guard was missed:\n{a}"
        );
        assert!(!a.contains("only_in_b"), "{a}");
    }

    #[test]
    fn postrm_removes_state_only_inside_the_purge_guard() {
        let text = composed(Script::Postrm, InitSystem::Systemd, InstallStrategy::Copy);

        let purge = guarded_block(&text, "\"$1\" = \"purge\"");
        assert!(!purge.is_empty(), "no purge block found in:\n{text}");
        assert!(
            purge.contains("rm -rf '/var/log/probe-app'"),
            "the log directory must be removed inside the purge guard:\n{purge}"
        );
        assert!(
            purge.contains("deluser"),
            "the account must be removed inside the purge guard:\n{purge}"
        );

        let remove = guarded_block(&text, "\"$1\" = \"remove\"");
        assert!(!remove.is_empty(), "no remove block found in:\n{text}");
        assert!(
            !remove.contains("rm -rf"),
            "removal must not destroy state; only purge does:\n{remove}"
        );
        assert!(
            !remove.contains("deluser"),
            "removal must not delete the account; only purge does:\n{remove}"
        );
    }

    /// Regression: a plain `start` in postinst paired with a prerm stopping only on `remove`
    /// meant an upgrade never restarted the service.
    #[test]
    fn an_upgrade_restarts_the_service() {
        let postinst = composed(Script::Postinst, InitSystem::Systemd, InstallStrategy::Copy);
        assert!(
            postinst.contains("[ -n \"$2\" ]"),
            "postinst must detect an upgrade, which dpkg signals through `$2`:\n{postinst}"
        );
        assert!(
            postinst.contains("restart"),
            "an upgrade must restart, not `start` an already-running unit:\n{postinst}"
        );

        // The other half: prerm stops on removal only, or the pairing is stop-then-start.
        let prerm = composed(Script::Prerm, InitSystem::Systemd, InstallStrategy::Copy);
        assert!(
            !guarded_block(&prerm, "\"$1\" = \"remove\"").is_empty(),
            "prerm must stop only on removal:\n{prerm}"
        );
        assert!(
            !prerm.contains("upgrade"),
            "prerm must not act on an upgrade:\n{prerm}"
        );
    }

    #[test]
    fn nothing_removes_the_dependency_directory_on_upgrade() {
        for script in Script::all() {
            let text = composed(script, InitSystem::Systemd, InstallStrategy::Copy);
            assert!(
                !text.contains("node_modules"),
                "{script:?} touches node_modules; an upgrade must not reinstall dependencies:\n{text}"
            );
        }
    }

    #[test]
    fn systemd_actions_are_guarded_on_systemd_running_not_on_the_binary() {
        let postinst = composed(Script::Postinst, InitSystem::Systemd, InstallStrategy::Copy);
        assert!(
            postinst.contains("[ -d /run/systemd/system ]"),
            "{postinst}"
        );
        assert!(
            !postinst.contains("hash systemctl"),
            "the binary's presence is not evidence systemd is running"
        );
    }

    #[test]
    fn the_unit_is_reloaded_before_it_is_started() {
        let postinst = composed(Script::Postinst, InitSystem::Systemd, InstallStrategy::Copy);
        let reload = postinst.find("daemon-reload").expect("reload present");
        let invoke = postinst
            .find("deb-systemd-invoke")
            .expect("invocation present");
        assert!(
            reload < invoke,
            "a changed unit must be reloaded before it is acted on"
        );
    }

    #[test]
    fn enabling_goes_through_the_debian_helper() {
        let postinst = composed(Script::Postinst, InitSystem::Systemd, InstallStrategy::Copy);
        assert!(postinst.contains("deb-systemd-helper enable"), "{postinst}");
        assert!(
            postinst.contains("was-enabled"),
            "an administrator's disable must survive an upgrade"
        );
        assert!(
            !postinst.contains("systemctl enable"),
            "a bare `systemctl enable` undoes a deliberate disable"
        );
    }

    #[test]
    fn starting_goes_through_the_policy_aware_invoker() {
        let postinst = composed(Script::Postinst, InitSystem::Systemd, InstallStrategy::Copy);
        assert!(postinst.contains("deb-systemd-invoke"), "{postinst}");
        assert!(
            !postinst.contains("systemctl start"),
            "`systemctl start` ignores policy-rc.d"
        );
        assert!(
            !postinst.contains("systemctl restart"),
            "`systemctl restart` ignores policy-rc.d"
        );
    }

    #[test]
    fn no_service_operation_can_fail_the_script() {
        for script in Script::all() {
            let text = composed(script, InitSystem::Auto, InstallStrategy::Copy);
            for line in text.lines() {
                let trimmed = line.trim();
                let is_service_call = trimmed.starts_with("deb-systemd")
                    || trimmed.starts_with("systemctl")
                    || trimmed.starts_with("initctl")
                    || trimmed.starts_with("invoke-rc.d")
                    || trimmed.starts_with("update-rc.d");
                if is_service_call {
                    assert!(
                        trimmed.ends_with("|| true"),
                        "{script:?}: `{trimmed}` can abort the installation"
                    );
                }
            }
        }
    }

    #[test]
    fn accounts_are_created_with_the_command_policy_directs() {
        let postinst = composed(Script::Postinst, InitSystem::Systemd, InstallStrategy::Copy);
        assert!(postinst.contains("adduser --system"), "{postinst}");
        assert!(postinst.contains("addgroup --system"), "{postinst}");
        assert!(
            !postinst.contains("useradd"),
            "raw useradd accepts names `adduser --system` refuses, and vice versa"
        );
    }

    #[test]
    fn account_existence_is_queried_precisely() {
        let postinst = composed(Script::Postinst, InitSystem::Systemd, InstallStrategy::Copy);
        assert!(postinst.contains("getent passwd 'probe-app'"), "{postinst}");
        assert!(
            !postinst.contains("getent passwd |"),
            "scanning the whole database matches substrings"
        );
    }

    #[test]
    fn the_default_strategy_installs_nothing_at_install_time() {
        for strategy in [InstallStrategy::Auto, InstallStrategy::Copy] {
            let postinst = composed(Script::Postinst, InitSystem::Systemd, strategy);
            assert!(
                !postinst.contains("npm install"),
                "{strategy:?} must not reach the network during installation:\n{postinst}"
            );
        }
    }

    #[test]
    fn the_install_time_strategy_is_still_available() {
        let postinst = composed(
            Script::Postinst,
            InitSystem::Systemd,
            InstallStrategy::NpmInstall,
        );
        assert!(postinst.contains("npm install"), "{postinst}");
        assert!(postinst.contains("--omit=dev"), "{postinst}");
    }

    #[test]
    fn every_composed_script_is_posix_sh_and_ends_successfully() {
        for script in Script::all() {
            for init in [
                InitSystem::Auto,
                InitSystem::Systemd,
                InitSystem::Sysv,
                InitSystem::Upstart,
                InitSystem::None,
            ] {
                let text = composed_raw(script, init, InstallStrategy::Copy);
                assert!(text.starts_with("#!/bin/sh\n"), "{script:?}/{init:?}");
                assert!(text.contains("set -e"), "{script:?}/{init:?}");
                assert!(
                    text.trim_end().ends_with("exit 0"),
                    "{script:?}/{init:?} must exit successfully on an unhandled action"
                );
            }
        }
    }

    #[test]
    fn script_names_match_what_dpkg_expects() {
        assert_eq!(
            Script::all().map(Script::name),
            ["preinst", "postinst", "prerm", "postrm"]
        );
    }
}
