//! The generated maintainer scripts must be valid shell. Composition is text assembly, so every
//! combination goes to a real shell, and to `shellcheck` where installed; a syntax error here
//! runs as root on users' machines.

use std::io::Write as _;
use std::process::{Command, Stdio};

use nativepkg::core::npm::{InitSystem, InstallStrategy};
use nativepkg::core::template::Variables;
use nativepkg::deb::scripts::{self, Script};

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

fn combinations() -> Vec<(Script, InitSystem, InstallStrategy)> {
    let mut out = Vec::new();
    for script in Script::all() {
        for init in [
            InitSystem::Auto,
            InitSystem::Systemd,
            InitSystem::Sysv,
            InitSystem::Upstart,
            InitSystem::None,
        ] {
            for strategy in [
                InstallStrategy::Auto,
                InstallStrategy::Copy,
                InstallStrategy::NpmInstall,
            ] {
                out.push((script, init, strategy));
            }
        }
    }
    out
}

fn compose(script: Script, init: InitSystem, strategy: InstallStrategy) -> String {
    scripts::compose(script, init, strategy, &vars())
        .unwrap_or_else(|e| panic!("{script:?}/{init:?}/{strategy:?} should compose: {e}"))
}

#[test]
fn the_sweep_covers_every_combination() {
    // four scripts x five init systems x three strategies
    assert_eq!(combinations().len(), 60);
}

#[test]
fn every_generated_script_parses_as_posix_shell() {
    for (script, init, strategy) in combinations() {
        let text = compose(script, init, strategy);
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
            "{script:?}/{init:?}/{strategy:?} is not valid shell: {}\n{text}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// Proves the check above can fail.
#[test]
fn the_shell_check_rejects_broken_shell() {
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
        .write_all(b"if [ 1 = 1 ]; then\n")
        .expect("write");
    let output = child.wait_with_output().expect("shell should finish");
    assert!(
        !output.status.success(),
        "`sh -n` accepted an unterminated `if`; the syntax check above proves nothing"
    );
}

#[test]
fn shellcheck_is_clean_when_available() {
    if Command::new("shellcheck")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("SKIP: `shellcheck` not on PATH; scripts were syntax-checked with `sh -n` only");
        return;
    }

    for (script, init, strategy) in combinations() {
        let text = compose(script, init, strategy);
        let mut child = Command::new("shellcheck")
            .args(["--shell=sh", "--severity=warning", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("shellcheck was available a moment ago");
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(text.as_bytes())
            .expect("write");
        let output = child.wait_with_output().expect("shellcheck should finish");
        assert!(
            output.status.success(),
            "{script:?}/{init:?}/{strategy:?}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

/// A header-only script means the composition silently did nothing.
#[test]
fn every_script_contains_more_than_a_header() {
    for (script, init, strategy) in combinations() {
        let text = compose(script, init, strategy);
        assert!(
            text.lines().count() >= 4,
            "{script:?}/{init:?}/{strategy:?} composed to almost nothing:\n{text}"
        );
    }
}

/// `chown -R` follows whatever already sits beneath the directory — on an upgrade, whatever
/// anyone with write access to `/var/log` left there; lintian: `recursive-privilege-change`.
/// Asserted on composed scripts so a snippet added later is covered.
#[test]
fn no_script_changes_ownership_recursively() {
    for script in Script::all() {
        for init in [
            InitSystem::Auto,
            InitSystem::Systemd,
            InitSystem::Sysv,
            InitSystem::Upstart,
            InitSystem::None,
        ] {
            for strategy in [InstallStrategy::Copy, InstallStrategy::NpmInstall] {
                let text = scripts::compose(script, init, strategy, &vars()).expect("composes");
                for line in text.lines().map(str::trim) {
                    if line.starts_with('#') {
                        continue;
                    }
                    assert!(
                        !(line.starts_with("chown") && line.contains("-R")),
                        "{script:?}/{init:?}/{strategy:?} changes ownership recursively:\n{line}"
                    );
                }
            }
        }
    }
}

/// Both branches must exist, or a sysv host gets no log directory at all.
#[test]
fn the_log_directory_is_created_with_and_without_systemd() {
    let text = scripts::compose(
        Script::Postinst,
        InitSystem::Auto,
        InstallStrategy::Copy,
        &vars(),
    )
    .expect("composes");
    let code: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .collect();
    assert!(
        code.iter()
            .any(|l| l.starts_with("systemd-tmpfiles --create")),
        "the systemd branch is missing:\n{text}"
    );
    assert!(
        code.iter()
            .any(|l| l.starts_with("mkdir -p '/var/log/probe-app'")),
        "the fallback branch is missing:\n{text}"
    );
}

#[test]
fn auto_composes_no_upstart_snippet() {
    for script in Script::all() {
        let text = scripts::compose(script, InitSystem::Auto, InstallStrategy::Copy, &vars())
            .expect("composes");
        assert!(
            !text.contains("initctl") && !text.contains("/etc/init/"),
            "{script:?} under auto carries Upstart logic:\n{text}"
        );
    }
    let text = scripts::compose(
        Script::Postinst,
        InitSystem::Upstart,
        InstallStrategy::Copy,
        &vars(),
    )
    .expect("composes");
    assert!(
        text.contains("initctl"),
        "explicit Upstart still composes:\n{text}"
    );
}

/// Two defects: the snippet still carried `--unsafe-perm`, which P2-10 promised to remove and
/// nothing tested; and two backends ran npm in the directory *above* `app/`, where it finds no
/// manifest and installs nothing.
#[test]
fn install_time_npm_runs_in_the_app_directory_without_running_scripts_as_root() {
    let text = scripts::compose(
        Script::Postinst,
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
