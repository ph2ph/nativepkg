//! RPM scriptlets must be valid shell, and must speak RPM's conventions rather than dpkg's.

use std::io::Write as _;
use std::process::{Command, Stdio};

use nativepkg::core::npm::{InitSystem, InstallStrategy};
use nativepkg::core::template::Variables;
use nativepkg::rpm::scriptlets::{self, Scriptlet};

fn vars() -> Variables {
    Variables::new()
        .with("package_name", "probe-app")
        .with("install_dir", "/usr/lib")
        .with("user", "probe-app")
        .with("group", "probe-app")
        .with("generator_version", "0.1.0")
        .with("install_binary", "npm")
        .with(
            "install_command",
            "npm install --omit=dev --ignore-scripts --no-audit --no-fund",
        )
}

fn composed(scriptlet: Scriptlet, init: InitSystem, strategy: InstallStrategy) -> String {
    scriptlets::compose(scriptlet, init, strategy, &vars()).expect("composition succeeds")
}

/// Comments explain why a Debian helper is *not* used, so absence is checked on code only.
fn code(script: &str) -> String {
    script
        .lines()
        .map(|line| match line.find('#') {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn every_combination() -> Vec<(Scriptlet, InitSystem, InstallStrategy, String)> {
    let mut out = Vec::new();
    for scriptlet in Scriptlet::all() {
        for init in [
            InitSystem::Auto,
            InitSystem::Systemd,
            InitSystem::Sysv,
            InitSystem::None,
        ] {
            for strategy in [InstallStrategy::Copy, InstallStrategy::NpmInstall] {
                out.push((
                    scriptlet,
                    init,
                    strategy,
                    composed(scriptlet, init, strategy),
                ));
            }
        }
    }
    out
}

#[test]
fn every_scriptlet_is_valid_shell() {
    for (scriptlet, init, strategy, text) in every_combination() {
        let mut child = Command::new("sh")
            .arg("-n")
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("a POSIX shell should be available");
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(text.as_bytes())
            .expect("write");
        let output = child.wait_with_output().expect("shell should finish");
        assert!(
            output.status.success(),
            "{scriptlet:?}/{init:?}/{strategy:?} is not valid shell:\n{text}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn no_debian_only_program_appears_in_an_rpm_scriptlet() {
    const DEBIAN_ONLY: &[&str] = &[
        "deb-systemd-helper",
        "deb-systemd-invoke",
        "adduser",
        "deluser",
        "addgroup",
        "delgroup",
        "update-rc.d",
        "invoke-rc.d",
        "dpkg",
    ];

    for (scriptlet, init, strategy, text) in every_combination() {
        let body = code(&text);
        for program in DEBIAN_ONLY {
            assert!(
                !body.contains(program),
                "{scriptlet:?}/{init:?}/{strategy:?} calls `{program}`, which RPM \
                 distributions do not have:\n{text}"
            );
        }
    }
}

/// dpkg's action words would never match RPM's numeric argument: dead code that looks alive.
#[test]
fn no_dpkg_action_word_is_dispatched_on() {
    for (scriptlet, init, strategy, text) in every_combination() {
        let body = code(&text);
        for word in ["configure", "abort-upgrade", "deconfigure", "purge"] {
            assert!(
                !body.contains(&format!("\"{word}\"")),
                "{scriptlet:?}/{init:?}/{strategy:?} dispatches on dpkg's `{word}`:\n{text}"
            );
        }
    }
}

#[test]
fn the_account_is_created_only_on_a_first_install() {
    let text = composed(Scriptlet::Pre, InitSystem::Systemd, InstallStrategy::Copy);
    assert!(text.contains("useradd"), "{text}");
    assert!(
        code(&text).contains("[ \"$1\" -eq 1 ]"),
        "creation must be guarded by RPM's install count, or an upgrade would retry it:\n{text}"
    );
}

#[test]
fn removal_acts_only_on_the_final_erase() {
    let preun = code(&composed(
        Scriptlet::Preun,
        InitSystem::Systemd,
        InstallStrategy::Copy,
    ));
    assert!(
        preun.contains("[ \"$1\" -eq 0 ]"),
        "an upgrade erases the old package too; acting there would stop a service that is \
         about to be replaced:\n{preun}"
    );
}

#[test]
fn an_upgrade_restarts_but_a_removal_does_not() {
    let postun = code(&composed(
        Scriptlet::Postun,
        InitSystem::Systemd,
        InstallStrategy::Copy,
    ));
    assert!(postun.contains("try-restart"), "{postun}");
    assert!(
        postun.contains("[ \"$1\" -ge 1 ]"),
        "restart must be conditional on the package still being installed:\n{postun}"
    );
}

#[test]
fn a_package_without_a_service_gets_no_lifecycle_logic() {
    for scriptlet in Scriptlet::all() {
        let text = composed(scriptlet, InitSystem::None, InstallStrategy::Copy);
        assert!(!text.contains("systemctl"), "{scriptlet:?}:\n{text}");
        assert!(!text.contains("useradd"), "{scriptlet:?}:\n{text}");
    }
}

#[test]
fn npm_install_survives_a_service_less_package() {
    let text = composed(
        Scriptlet::Post,
        InitSystem::None,
        InstallStrategy::NpmInstall,
    );
    assert!(text.contains("npm install"), "{text}");
}

/// A non-zero scriptlet aborts the transaction; `set -e` would turn every lifecycle failure
/// into a failed install.
#[test]
fn lifecycle_failures_cannot_abort_the_transaction() {
    // Every external program, not only lifecycle ones: a list of `systemctl` and `chkconfig`
    // could not see `useradd` and `groupadd` called unguarded — the shape that made a Debian
    // package fail to install, found by a container run.
    const SPAWNS: &[&str] = &[
        "systemctl",
        "chkconfig",
        "service",
        "useradd",
        "groupadd",
        "userdel",
        "groupdel",
        "install",
        "npm",
    ];

    for (scriptlet, init, strategy, text) in every_combination() {
        assert!(
            !text.contains("set -e"),
            "{scriptlet:?}/{init:?}/{strategy:?} uses `set -e`:\n{text}"
        );
        for line in code(&text).lines() {
            let call = line.trim();
            if SPAWNS.iter().any(|program| call.starts_with(program)) {
                assert!(
                    call.ends_with("|| :") || call.ends_with("|| true"),
                    "`{call}` can abort the transaction in {scriptlet:?}/{init:?}"
                );
            }
        }
    }
}

/// Two defects: the snippet still carried `--unsafe-perm`, which P2-10 promised to remove and
/// nothing tested; and two backends ran npm in the directory *above* `app/`, where it finds no
/// manifest and installs nothing.
#[test]
fn install_time_npm_runs_in_the_app_directory_without_running_scripts_as_root() {
    let text = scriptlets::compose(
        Scriptlet::Post,
        InitSystem::None,
        InstallStrategy::NpmInstall,
        &vars(),
    )
    .expect("composes");
    let code: String = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(code.contains("npm install"), "{text}");
    assert!(
        !code.contains("--unsafe-perm"),
        "P2-10: never as an unsafe root:\n{text}"
    );
    assert!(
        code.contains("--ignore-scripts"),
        "third-party hooks must not run as root:\n{text}"
    );
    assert!(
        code.contains("/probe-app/app' && npm install"),
        "npm must run where package.json is, in `app/`:\n{text}"
    );
}

/// `%systemd_post` is preset-only: under booted Fedora the unit was enabled and dead until
/// reboot while the `.deb` came up at once. The start is confined to a first install; on
/// upgrade `%postun` does `try-restart`, which respects a stopped service.
#[test]
fn a_first_install_starts_the_unit_and_an_upgrade_does_not_start_it_again() {
    let post = code(&composed(
        Scriptlet::Post,
        InitSystem::Systemd,
        InstallStrategy::Auto,
    ));
    let first_install = post
        .split("if [ \"$1\" -eq 1 ]; then")
        .nth(1)
        .and_then(|rest| rest.split("\nfi").next())
        .expect("a first-install block");
    assert!(
        first_install.contains("systemctl preset"),
        "{first_install}"
    );
    assert!(first_install.contains("systemctl start"), "{first_install}");
    let outside = post.replace(first_install, "");
    assert!(
        !outside.contains("systemctl start"),
        "start must not run unconditionally:\n{outside}"
    );
}

/// The predicate the CLI ships the preset policy on must be the one that selects the snippet.
#[test]
fn the_preset_predicate_agrees_with_the_snippet_selection() {
    use nativepkg::rpm::scriptlets::{snippets_for, uses_preset};
    for init in [
        InitSystem::Auto,
        InitSystem::Systemd,
        InitSystem::Sysv,
        InitSystem::Upstart,
        InitSystem::None,
    ] {
        let presets =
            snippets_for(Scriptlet::Post, init, InstallStrategy::Auto).contains(&"post-systemd");
        assert_eq!(uses_preset(init), presets, "{init:?}");
    }
}
