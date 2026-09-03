//! One plan, three packages, each in its own dialect. The only place all three backends are
//! visible at once, so the only place the plan's format-agnosticism can be tested: a backend
//! crate cannot depend on a sibling.

use std::path::{Path, PathBuf};

use nativepkg::core::npm::{InitSystem, InstallStrategy};
use nativepkg::core::plan::BuildPlan;
use nativepkg::core::resolve::{Overrides, ResolvedConfig};
use nativepkg::core::{Manifest, build, resolve};
use nativepkg::format::Format;

const MANIFEST: &str = r#"{
  "name": "probe-app",
  "version": "1.2.3",
  "description": "a probe",
  "author": "A <a@example.com>",
  "nativepkg": { "entrypoints": { "daemon": "app.js" } }
}"#;

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("nativepkg-cli-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("lib")).expect("fixture tree");
        std::fs::write(root.join("package.json"), MANIFEST).expect("manifest");
        let entry = root.join("app.js");
        std::fs::write(&entry, "#!/usr/bin/env node\nconsole.log('hi');\n").expect("entry point");
        executable(&entry);
        std::fs::write(root.join("lib").join("one.js"), "module.exports = 1;\n").expect("lib");
        Self { root }
    }

    fn plan(&self) -> (BuildPlan, ResolvedConfig) {
        let manifest = Manifest::from_path(self.root.join("package.json")).expect("manifest");
        let (config, _) = resolve(&manifest, &Overrides::default()).expect("resolves");
        let inputs = [
            PathBuf::from("package.json"),
            PathBuf::from("app.js"),
            PathBuf::from("lib"),
        ];
        let (plan, _, _) = build::plan(&config, &self.root, &inputs).expect("plans");
        (plan, config)
    }

    fn out(&self) -> PathBuf {
        self.root.join("out")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn build_all(fixture: &Fixture) -> Vec<(Format, PathBuf)> {
    let (plan, config) = fixture.plan();
    let mut produced = Vec::new();

    for format in Format::ALL {
        let variables = format.variables(&config, "0.1.0").expect("vocabulary");
        let path = format
            .build(&plan, &config, &variables, &fixture.out())
            .unwrap_or_else(|e| panic!("{format} should build: {e:?}"));
        produced.push((format, path));
    }
    produced
}

#[test]
fn one_plan_produces_every_format_without_being_touched() {
    let fixture = Fixture::new("one-plan");
    let (plan, config) = fixture.plan();

    let before = format!("{plan:?}");
    for format in Format::ALL {
        let variables = format.variables(&config, "0.1.0").expect("vocabulary");
        let path = format
            .build(&plan, &config, &variables, &fixture.out())
            .unwrap_or_else(|e| panic!("{format} should build: {e:?}"));
        assert!(path.exists(), "{format} wrote nothing");
    }

    assert_eq!(
        before,
        format!("{plan:?}"),
        "a backend mutated the plan the others still have to read"
    );
}

/// Binding one spelling for every format is how a package said `amd64` where its header said
/// `x86_64`.
#[test]
fn each_format_renders_its_own_architecture_spelling() {
    let fixture = Fixture::new("arch-spelling");
    let (_, mut config) = fixture.plan();
    config.architecture = "amd64".to_owned();

    assert_eq!(Format::Deb.architecture_of(&config).expect("deb"), "amd64");
    assert_eq!(Format::Rpm.architecture_of(&config).expect("rpm"), "x86_64");
    assert_eq!(
        Format::Arch.architecture_of(&config).expect("arch"),
        "x86_64"
    );

    // The template vocabulary carries that spelling — the binding that was wrong.
    for (format, expected) in [
        (Format::Deb, "amd64"),
        (Format::Rpm, "x86_64"),
        (Format::Arch, "x86_64"),
    ] {
        let variables = format.variables(&config, "0.1.0").expect("vocabulary");
        let rendered = variables
            .resolve("package_architecture")
            .expect("the variable exists");
        assert_eq!(rendered, expected, "{format}");
    }
}

#[test]
fn each_format_renders_its_own_version_spelling() {
    let fixture = Fixture::new("version-spelling");
    let manifest = Manifest::from_path(fixture.root.join("package.json")).expect("manifest");
    let overrides = Overrides {
        epoch: Some(1),
        ..Overrides::default()
    };
    let (config, _) = resolve(&manifest, &overrides).expect("resolves");

    assert_eq!(Format::Deb.version_of(&config), "1:1.2.3");
    assert_eq!(Format::Rpm.version_of(&config), "1.2.3");
    assert_eq!(Format::Arch.version_of(&config), "1.2.3");
}

/// Read out of the packages, decompressed first. An earlier version scanned raw bytes, which
/// works for RPM (scriptlets sit in the uncompressed header) and proves nothing for Arch, where
/// `.INSTALL` is inside the zstd stream.
#[test]
fn no_format_ships_another_formats_tooling() {
    let fixture = Fixture::new("no-cross-tooling");
    let produced = build_all(&fixture);

    for (format, path) in produced {
        let scripts = scripts_in(format, &path);
        assert!(
            !scripts.is_empty(),
            "{format}: no maintainer script was found to inspect, so this test would pass \
             vacuously"
        );

        let forbidden: &[&str] = match format {
            // Debian's own tooling is correct in a .deb; the others must not carry it.
            Format::Deb => &["systemctl preset", "useradd", "chkconfig"],
            Format::Rpm | Format::Arch => &[
                "deb-systemd-helper",
                "deb-systemd-invoke",
                "adduser",
                "addgroup",
                "update-rc.d",
                "invoke-rc.d",
            ],
        };

        // Comments explain why a foreign tool is *not* used, so compare against code only.
        let body = code(&scripts);
        for program in forbidden {
            assert!(
                !body.contains(program),
                "the {format} package contains `{program}`, which belongs to another \
                 format:\n{scripts}"
            );
        }
    }
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

fn scripts_in(format: Format, path: &Path) -> String {
    let bytes = std::fs::read(path).expect("the package");

    match format {
        Format::Deb => {
            let package = nativepkg::deb::read::parse(&bytes).expect("a readable .deb");
            package
                .script_bodies
                .values()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        }
        // RPM keeps its scriptlets in the header, outside the compressed payload.
        Format::Rpm => String::from_utf8_lossy(&bytes).into_owned(),
        Format::Arch => {
            let tar = zstd::decode_all(&bytes[..]).expect("a readable zstd stream");
            let mut archive = tar::Archive::new(&tar[..]);
            let mut found = String::new();
            for entry in archive.entries().expect("entries") {
                let mut entry = entry.expect("an entry");
                let name = entry.path().expect("a path").to_string_lossy().into_owned();
                if name.ends_with(".INSTALL") {
                    use std::io::Read as _;
                    entry.read_to_string(&mut found).expect("readable");
                }
            }
            found
        }
    }
}

#[test]
fn a_package_carries_only_the_init_system_it_was_built_for() {
    let fixture = Fixture::new("one-init");
    let (plan, mut config) = fixture.plan();
    config.init = InitSystem::Systemd;
    config.install_strategy = InstallStrategy::Copy;

    let variables = Format::Deb.variables(&config, "0.1.0").expect("vocabulary");
    let path = Format::Deb
        .build(&plan, &config, &variables, &fixture.out())
        .expect("builds");

    let text = String::from_utf8_lossy(&std::fs::read(&path).expect("the package")).into_owned();
    assert!(
        !text.contains("update-rc.d") && !text.contains("initctl"),
        "a systemd package carries logic for another init system"
    );
}

#[test]
fn a_format_reports_what_it_cannot_honour() {
    assert!(Format::Arch.unsupported(InitSystem::Sysv).is_some());
    assert!(Format::Arch.unsupported(InitSystem::Upstart).is_some());
    assert!(Format::Arch.unsupported(InitSystem::Systemd).is_none());

    assert!(Format::Rpm.unsupported(InitSystem::Upstart).is_some());
    assert!(Format::Rpm.unsupported(InitSystem::Sysv).is_none());

    // Debian is the one format that genuinely supports all three.
    for init in [InitSystem::Systemd, InitSystem::Sysv, InitSystem::Upstart] {
        assert!(Format::Deb.unsupported(init).is_none(), "{init:?}");
    }
}

#[test]
// Names are generated lowercase, and `.pkg.tar.zst` is not a single extension.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn each_format_writes_its_own_file_name() {
    let fixture = Fixture::new("file-names");
    let produced = build_all(&fixture);

    let names: Vec<String> = produced
        .iter()
        .map(|(_, path)| {
            Path::new(path)
                .file_name()
                .expect("a name")
                .to_string_lossy()
                .into_owned()
        })
        .collect();

    assert!(names.iter().any(|n| n.ends_with(".deb")), "{names:?}");
    assert!(names.iter().any(|n| n.ends_with(".rpm")), "{names:?}");
    assert!(
        names.iter().any(|n| n.ends_with(".pkg.tar.zst")),
        "{names:?}"
    );
}

/// `collect` planned the link and nothing generated its target, so every package shipped a
/// dangling symlink; no test resolved it.
#[test]
fn the_executable_symlink_points_at_something_in_the_package() {
    let fixture = Fixture::new("no-dangling-link");
    let (plan, config) = fixture.plan();

    for format in Format::ALL {
        let variables = format.variables(&config, "0.1.0").expect("vocabulary");
        let generated: Vec<_> = nativepkg::core::build::service_files(&config)
            .expect("placement")
            .into_iter()
            .map(|service| {
                let source = nativepkg::core::template::builtin(service.template)
                    .expect("a built-in template");
                let text = nativepkg::core::template::render(service.template, source, &variables)
                    .expect("renders");
                (service, text)
            })
            .collect();
        let derived = nativepkg::core::build::with_generated(&plan, generated).expect("plan");

        let destinations: Vec<String> = derived
            .files
            .iter()
            .map(|f| f.destination.as_str().to_owned())
            .collect();

        let link = derived
            .files
            .iter()
            .find(|f| f.destination.as_str() == "/usr/bin/probe-app")
            .expect("the wrapper symlink is planned");

        let nativepkg::core::plan::EntryKind::Symlink { target } = &link.kind else {
            panic!("{format}: the /usr/bin entry should be a symlink");
        };

        // The target is relative to the link's own directory, as `dh_link` writes them.
        let resolved = format!("/usr/{}", target.trim_start_matches("../"));
        assert!(
            destinations.contains(&resolved),
            "{format}: `/usr/bin/probe-app` points at `{target}` (`{resolved}`), which the \
             package does not contain:\n{destinations:#?}"
        );
    }
}

/// systemd recreates a declared directory at boot, so a cleaned `/var/log` does not stop the
/// service. Before, ownership came from `chown -R` in `postinst`, which lintian flags as
/// `recursive-privilege-change`.
#[test]
fn a_systemd_package_declares_its_log_directory_to_tmpfiles() {
    let fixture = Fixture::new("tmpfiles");
    let (_, config) = fixture.plan();

    let placed = nativepkg::core::build::service_files(&config).expect("placement");
    let fragment = placed
        .iter()
        .find(|s| s.template == "tmpfiles.conf")
        .expect("a tmpfiles.d fragment is placed for a systemd package");
    assert_eq!(
        fragment.destination.as_str(),
        "/usr/lib/tmpfiles.d/probe-app.conf"
    );

    let variables = Format::Deb.variables(&config, "0.1.0").expect("vocabulary");
    let source = nativepkg::core::template::builtin("tmpfiles.conf").expect("built in");
    let text =
        nativepkg::core::template::render("tmpfiles.conf", source, &variables).expect("renders");
    let declaration = text
        .lines()
        .find(|l| l.starts_with("d "))
        .expect("a directory declaration");
    assert_eq!(
        declaration,
        "d /var/log/probe-app 0755 probe-app probe-app -"
    );
}

#[test]
fn a_systemd_package_contains_its_unit() {
    let fixture = Fixture::new("unit-present");
    let (_, config) = fixture.plan();

    let placed = nativepkg::core::build::service_files(&config).expect("placement");
    let destinations: Vec<&str> = placed.iter().map(|s| s.destination.as_str()).collect();

    assert!(
        destinations.contains(&"/usr/lib/systemd/system/probe-app.service"),
        "{destinations:?}"
    );
    // `/lib/systemd/system` was flagged in the triage; it must not come back.
    assert!(
        !destinations.iter().any(|d| d.starts_with("/lib/systemd")),
        "{destinations:?}"
    );
}

/// Debian removed Upstart in stretch and Ubuntu in 15.04; every `auto` package shipped a dead
/// `/etc/init/<pkg>.conf` that lintian flags.
#[test]
fn auto_does_not_include_upstart_but_an_explicit_request_does() {
    let fixture = Fixture::new("auto-no-upstart");
    let (_, mut config) = fixture.plan();

    config.init = InitSystem::Auto;
    let auto: Vec<&str> = nativepkg::core::build::service_files(&config)
        .expect("placement")
        .iter()
        .map(|s| s.template)
        .collect::<Vec<_>>()
        .into_iter()
        .collect();
    assert!(auto.contains(&"systemd.service"), "{auto:?}");
    assert!(
        auto.contains(&"sysv-init"),
        "auto keeps a System V fallback: {auto:?}"
    );
    assert!(
        !auto.contains(&"upstart.conf"),
        "auto must not ship Upstart: {auto:?}"
    );

    config.init = InitSystem::Upstart;
    let explicit: Vec<&str> = nativepkg::core::build::service_files(&config)
        .expect("placement")
        .iter()
        .map(|s| s.template)
        .collect();
    assert!(
        explicit.contains(&"upstart.conf"),
        "a manifest that names Upstart still gets it: {explicit:?}"
    );
}

fn executable(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}
