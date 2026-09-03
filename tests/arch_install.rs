//! The `.INSTALL` file must be valid shell, must define the functions `pacman` calls, and must
//! not carry another distribution's tooling.

use std::io::Write as _;
use std::process::{Command, Stdio};

use nativepkg::arch::install;
use nativepkg::core::npm::{InitSystem, InstallStrategy};
use nativepkg::core::template::Variables;

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

fn composed(init: InitSystem, strategy: InstallStrategy) -> Option<String> {
    install::compose(init, strategy, &vars()).expect("composition succeeds")
}

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

#[test]
fn the_scriptlet_is_valid_shell() {
    for init in [InitSystem::Auto, InitSystem::Systemd, InitSystem::None] {
        for strategy in [InstallStrategy::Copy, InstallStrategy::NpmInstall] {
            let Some(text) = composed(init, strategy) else {
                continue;
            };
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
                "{init:?}/{strategy:?} is not valid shell:\n{text}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

/// A misnamed function is a hook that silently never runs.
#[test]
fn pacman_finds_the_functions_it_calls() {
    let text = composed(InitSystem::Systemd, InstallStrategy::Copy).expect("a service package");
    for function in ["post_install", "post_upgrade", "pre_remove", "post_remove"] {
        assert!(
            text.contains(&format!("{function}() {{")),
            "`{function}` is missing:\n{text}"
        );
    }
}

#[test]
fn no_other_distributions_tooling_appears() {
    const FOREIGN: &[&str] = &[
        "deb-systemd-helper",
        "deb-systemd-invoke",
        "adduser",
        "deluser",
        "update-rc.d",
        "invoke-rc.d",
        "chkconfig",
        "dpkg",
    ];
    for init in [InitSystem::Auto, InitSystem::Systemd] {
        for strategy in [InstallStrategy::Copy, InstallStrategy::NpmInstall] {
            let text = composed(init, strategy).expect("a service package");
            let body = code(&text);
            for program in FOREIGN {
                assert!(
                    !body.contains(program),
                    "{init:?}/{strategy:?} calls `{program}`:\n{text}"
                );
            }
        }
    }
}

#[test]
fn nothing_dispatches_on_a_positional_argument() {
    let text = composed(InitSystem::Systemd, InstallStrategy::Copy).expect("a service package");
    let body = code(&text);
    assert!(!body.contains("$1"), "{text}");
    assert!(!body.contains("$2"), "{text}");
}

/// On an upgrade even `preset` would undo a deliberate disable, so it must not run there.
#[test]
fn an_upgrade_does_not_re_enable_a_disabled_unit() {
    let text = composed(InitSystem::Systemd, InstallStrategy::Copy).expect("a service package");
    let upgrade = text
        .split("post_upgrade() {")
        .nth(1)
        .and_then(|rest| rest.split("\n}").next())
        .expect("post_upgrade body");
    assert!(
        !upgrade.contains("preset") && !upgrade.contains("enable"),
        "post_upgrade must not enable:\n{upgrade}"
    );
    assert!(upgrade.contains("try-restart"), "{upgrade}");

    let install = text
        .split("post_install() {")
        .nth(1)
        .and_then(|rest| rest.split("\n}").next())
        .expect("post_install body");
    assert!(
        install.contains("preset"),
        "a first install is where enabling belongs:\n{install}"
    );
}

#[test]
fn removal_happens_in_the_order_the_filesystem_requires() {
    let text = composed(InitSystem::Systemd, InstallStrategy::Copy).expect("a service package");
    let pre = text.find("pre_remove() {").expect("pre_remove");
    let post = text.find("post_remove() {").expect("post_remove");
    assert!(pre < post, "{text}");
    assert!(text[pre..post].contains("stop"), "{text}");
    assert!(text[post..].contains("userdel"), "{text}");
}

#[test]
fn a_package_with_no_hooks_has_no_install_file() {
    assert!(composed(InitSystem::None, InstallStrategy::Copy).is_none());
    assert!(composed(InitSystem::None, InstallStrategy::NpmInstall).is_some());
}

#[test]
fn an_init_system_arch_does_not_have_is_reported() {
    assert_eq!(install::unsupported_init(InitSystem::Sysv), Some("sysv"));
    assert_eq!(
        install::unsupported_init(InitSystem::Upstart),
        Some("upstart")
    );
    assert_eq!(install::unsupported_init(InitSystem::Systemd), None);
    assert_eq!(install::unsupported_init(InitSystem::Auto), None);
    assert_eq!(install::unsupported_init(InitSystem::None), None);

    // And no half-composed lifecycle logic for it.
    let text = composed(InitSystem::Sysv, InstallStrategy::Copy);
    assert!(text.is_none(), "{text:?}");
}

/// `post_install` used `try-restart`, which does nothing when the unit is not running — always
/// the case on a first install. The unit was enabled and never came up.
#[test]
fn a_fresh_install_starts_the_service() {
    let text = composed(InitSystem::Systemd, InstallStrategy::Copy).expect("a service package");
    let install = text
        .split("post_install() {")
        .nth(1)
        .and_then(|rest| rest.split("\n}").next())
        .expect("post_install body");
    let body = code(install);

    assert!(
        body.contains("systemctl start"),
        "post_install must start the unit:\n{install}"
    );
    assert!(
        !body.contains("try-restart"),
        "`try-restart` does nothing when the unit is not running, which is every first \
         install:\n{install}"
    );

    // An upgrade keeps `try-restart`: a stopped service there was stopped by the administrator.
    let upgrade = text
        .split("post_upgrade() {")
        .nth(1)
        .and_then(|rest| rest.split("\n}").next())
        .expect("post_upgrade body");
    let upgrade_body = code(upgrade);
    assert!(upgrade_body.contains("try-restart"), "{upgrade}");
    assert!(
        !upgrade_body.contains("systemctl start"),
        "an upgrade must not start a service the administrator stopped:\n{upgrade}"
    );
}

/// Two defects: the snippet still carried `--unsafe-perm`, which P2-10 promised to remove and
/// nothing tested; and two backends ran npm in the directory *above* `app/`, where it finds no
/// manifest and installs nothing.
#[test]
fn install_time_npm_runs_in_the_app_directory_without_running_scripts_as_root() {
    let text = install::compose(InitSystem::None, InstallStrategy::NpmInstall, &vars())
        .expect("composes")
        .expect("npm-install produces a file");
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
