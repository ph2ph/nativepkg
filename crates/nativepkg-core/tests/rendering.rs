//! The generated service files carry the corrections this tool makes, and reference nothing it
//! does not actually ship.

use std::path::{Path, PathBuf};

use nativepkg_core::resolve::Overrides;
use nativepkg_core::template::{self, Variables};
use nativepkg_core::{Manifest, resolve};

/// Templates that used to be built in and are now generated elsewhere; none may come back as a
/// builtin, or one artefact would have two sources.
const SUPERSEDED: &[&str] = &["control", "preinst", "postinst", "prerm", "postrm"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root should exist")
}

fn simple_fixture_variables() -> Variables {
    let manifest = Manifest::from_path(repo_root().join("tests/fixtures/simple/package.json"))
        .expect("fixture manifest should parse");
    let (config, _) = resolve(&manifest, &Overrides::default()).expect("fixture should resolve");
    Variables::for_config(
        &config,
        "0.1.0",
        config.version.deb(),
        config
            .architecture_parsed()
            .expect("fixture architecture")
            .deb(),
    )
}

/// Strips ini/shell comments so a check looks at directives, not documentation.
fn code(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_unit_references_no_documentation_the_tool_does_not_ship() {
    let variables = simple_fixture_variables();
    let source = template::builtin("systemd.service").expect("built-in");
    let rendered = template::render("systemd.service", source, &variables).expect("renders");
    assert!(
        !code(&rendered).contains("Documentation="),
        "nothing in this tool generates a man page:\n{rendered}"
    );
}

#[test]
fn the_rewritten_unit_carries_its_corrections() {
    let variables = simple_fixture_variables();
    let source = template::builtin("systemd.service").expect("built-in");
    let rendered = template::render("systemd.service", source, &variables).expect("renders");
    let unit = code(&rendered);

    for required in [
        "After=network-online.target",
        "Wants=network-online.target",
        "Group=",
        "NoNewPrivileges=yes",
        "ProtectSystem=strict",
        "PrivateTmp=yes",
        "ReadWritePaths=",
        "RestartSec=",
    ] {
        assert!(
            unit.contains(required),
            "unit is missing `{required}`:\n{unit}"
        );
    }
    assert!(
        !unit.contains("PermissionsStartOnly"),
        "the directive was removed from systemd years ago"
    );
    assert!(
        !unit.contains("Requires=network.target"),
        "a bare requirement expresses no ordering"
    );
}

/// What allows `sudo` to leave the default runtime dependencies.
#[test]
fn the_rewritten_upstart_job_drops_privileges_without_sudo() {
    let variables = simple_fixture_variables();
    let source = template::builtin("upstart.conf").expect("built-in");
    let rendered = template::render("upstart.conf", source, &variables).expect("renders");
    let job = code(&rendered);
    assert!(job.contains("setuid "), "{job}");
    assert!(job.contains("setgid "), "{job}");
    assert!(!job.contains("sudo"), "{job}");
}

/// Two sources for one artefact is how they drift apart.
#[test]
fn superseded_templates_are_no_longer_builtins() {
    for name in SUPERSEDED {
        assert!(
            template::builtin(name).is_err(),
            "`{name}` is generated elsewhere and must not also be a template"
        );
        assert!(!template::builtin_names().contains(name));
    }
}

/// Text assertions say the unit *contains* the right directives; only systemd says it is
/// *valid*.
#[test]
fn systemd_accepts_the_generated_unit_when_available() {
    use std::process::Command;

    if Command::new("systemd-analyze")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("SKIP: `systemd-analyze` not on PATH; the unit was only checked textually");
        return;
    }

    // A name no system man page can coincide with: the fixture is `simple`, this host ships
    // `tc-simple.8`, and `man simple(8)` resolved to it, so a stray `Documentation=` went
    // unnoticed.
    let variables = simple_fixture_variables().with("package_name", "nativepkg-verify-probe-x9");
    let source = template::builtin("systemd.service").expect("built-in");
    let unit = template::render("systemd.service", source, &variables).expect("renders");

    let dir = std::env::temp_dir().join("nativepkg-unit-verify");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("nativepkg-verify-probe-x9.service");
    std::fs::write(&path, &unit).expect("write");

    let output = Command::new("systemd-analyze")
        .arg("verify")
        .arg(&path)
        .output()
        .expect("systemd-analyze was available a moment ago");
    std::fs::remove_file(&path).ok();

    // `verify` also reports on the host's own units; only lines naming our file count.
    let stderr = String::from_utf8_lossy(&output.stderr);
    let ours: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains("nativepkg-verify-probe-x9"))
        // The executable does not exist on this machine; that is the fixture, not the unit.
        .filter(|line| !line.contains("is not executable"))
        .collect();
    assert!(
        ours.is_empty(),
        "systemd rejected the generated unit:\n{}",
        ours.join("\n")
    );
}
