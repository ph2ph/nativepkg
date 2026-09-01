//! A dry run must describe the package that would actually be built, and write nothing.

use std::path::PathBuf;
use std::process::Command;

fn binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("test binary path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join("nativepkg")
}

struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("nativepkg-dry-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("lib")).expect("tree");
        // The binary reads `.nativepkg`; a package.json is written too, purely as a file to be
        // packaged, to prove it is shipped without being read for metadata.
        std::fs::write(
            root.join(".nativepkg"),
            r#"{"package_name":"probe-app","version":"1.2.3","description":"d",
                "maintainer":"A <a@example.com>",
                "entrypoints":{"daemon":"app.js"}}"#,
        )
        .expect("config");
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"ignored","version":"0.0.0"}"#,
        )
        .expect("payload package.json");
        // A shebang and the bit: the planner refuses an entry point the kernel cannot run, and
        // this fixture was the first thing it caught.
        let entry = root.join("app.js");
        std::fs::write(&entry, "#!/usr/bin/env node\nconsole.log(1);\n").expect("entry");
        executable(&entry);
        std::fs::write(root.join("lib").join("a.js"), "module.exports=1;\n").expect("lib");
        Self { root }
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_dry_run_writes_nothing_and_emits_parseable_json() {
    let project = Project::new("json");
    let out = project.root.join("out");

    let result = Command::new(binary())
        .current_dir(&project.root)
        .args([
            "--format",
            "deb,rpm,arch",
            "--dry-run",
            "--json",
            "--quiet",
            "--output-dir",
        ])
        .arg(&out)
        .args(["app.js", "lib", "package.json"])
        .output()
        .expect("the binary should run");

    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        !out.exists(),
        "a dry run created the output directory, so it did more than describe"
    );

    let document: serde_json::Value =
        serde_json::from_slice(&result.stdout).expect("the output should be JSON");

    assert_eq!(document["package"], "probe-app");
    assert_eq!(
        document["formats"],
        serde_json::json!(["deb", "rpm", "arch"])
    );

    let destinations: Vec<&str> = document["files"]
        .as_array()
        .expect("files is a list")
        .iter()
        .map(|f| f["destination"].as_str().expect("a destination"))
        .collect();

    // Generated files must be described too, or the dry run reports a package missing its unit,
    // wrapper and defaults file — not the package that would be built.
    for expected in [
        "/usr/bin/probe-app",
        "/usr/lib/probe-app/bin/probe-app",
        "/usr/lib/systemd/system/probe-app.service",
        "/etc/default/probe-app",
    ] {
        assert!(
            destinations.contains(&expected),
            "`{expected}` is missing from the dry run:\n{destinations:#?}"
        );
    }
}

#[test]
fn the_dry_run_reports_a_version_per_format() {
    let project = Project::new("versions");

    let result = Command::new(binary())
        .current_dir(&project.root)
        .args([
            "--format",
            "deb,rpm",
            "--epoch",
            "1",
            "--dry-run",
            "--json",
            "--quiet",
            "app.js",
            "package.json",
        ])
        .output()
        .expect("the binary should run");

    let document: serde_json::Value =
        serde_json::from_slice(&result.stdout).expect("the output should be JSON");
    let versions = document["versions"].as_array().expect("a list");

    let deb = versions
        .iter()
        .find(|v| v["format"] == "deb")
        .expect("deb present");
    let rpm = versions
        .iter()
        .find(|v| v["format"] == "rpm")
        .expect("rpm present");

    assert_eq!(deb["version"], "1:1.2.3");
    assert_eq!(rpm["version"], "1.2.3", "RPM carries the epoch separately");
}

#[test]
fn a_refused_option_exits_two_and_writes_nothing() {
    let project = Project::new("refused");
    let out = project.root.join("out");

    let result = Command::new(binary())
        .current_dir(&project.root)
        .args(["--no-md5sums", "--output-dir"])
        .arg(&out)
        .arg("app.js")
        .output()
        .expect("the binary should run");

    assert_eq!(
        result.status.code(),
        Some(2),
        "a refused option needs an exit status distinct from a build failure"
    );
    assert!(!out.exists());

    let message = String::from_utf8_lossy(&result.stderr);
    assert!(message.contains("--no-md5sums"), "{message}");
    assert!(
        message.contains("policy"),
        "the reason must be given: {message}"
    );
}

#[test]
// Names are generated lowercase, and `.pkg.tar.zst` is not a single extension.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn one_invocation_writes_one_package_per_format() {
    let project = Project::new("three");
    let out = project.root.join("out");

    let result = Command::new(binary())
        .current_dir(&project.root)
        .args(["--format", "deb,rpm,arch", "--output-dir"])
        .arg(&out)
        .args(["app.js", "lib", "package.json"])
        .output()
        .expect("the binary should run");

    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let mut written: Vec<String> = std::fs::read_dir(&out)
        .expect("output directory")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    written.sort();

    assert_eq!(written.len(), 3, "{written:?}");
    assert!(written.iter().any(|n| n.ends_with(".deb")), "{written:?}");
    assert!(written.iter().any(|n| n.ends_with(".rpm")), "{written:?}");
    assert!(
        written.iter().any(|n| n.ends_with(".pkg.tar.zst")),
        "{written:?}"
    );

    // Every written path is echoed one per line, so a caller need not guess the naming.
    let printed = String::from_utf8_lossy(&result.stdout);
    assert_eq!(printed.lines().count(), 3, "{printed}");
}

/// The backend accepted a `triggers` member from the start, but no flag or manifest key could
/// set it: the field defaulted to `None` at its only construction site.
#[test]
fn a_triggers_file_reaches_the_control_archive_verbatim() {
    let project = Project::new("triggers");
    let triggers = project.root.join("my.triggers");
    std::fs::write(&triggers, "interest-noawait /usr/lib/probe-app\n").expect("write");
    let out = project.root.join("out");

    let result = Command::new(binary())
        .current_dir(&project.root)
        .args(["--quiet", "--format", "deb", "--output-dir"])
        .arg(&out)
        .args([
            "--maintainer",
            "A <a@example.com>",
            "--triggers-file",
            "my.triggers",
        ])
        .args(["package.json", "app.js", "lib"])
        .output()
        .expect("the binary should run");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let deb = std::fs::read_dir(&out)
        .expect("output")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "deb"))
        .expect("a .deb");
    let package =
        nativepkg_deb::read::parse(&std::fs::read(&deb).expect("bytes")).expect("readable");
    assert_eq!(
        package.script_bodies.get("triggers").map(String::as_str),
        Some("interest-noawait /usr/lib/probe-app\n")
    );
}

/// Found from the resolver, not a list of conventional names — which once omitted `app.js`.
#[test]
fn a_bare_invocation_packages_the_entrypoints() {
    let project = Project::new("bare");
    let result = Command::new(binary())
        .current_dir(&project.root)
        .args([
            "--dry-run",
            "--json",
            "--quiet",
            "--maintainer",
            "A <a@example.com>",
        ])
        .output()
        .expect("the binary should run");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let document: serde_json::Value = serde_json::from_slice(&result.stdout).expect("JSON");
    let destinations: Vec<&str> = document["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|f| f["destination"].as_str().expect("destination"))
        .collect();
    assert!(
        destinations.contains(&"/usr/lib/probe-app/app/app.js"),
        "the daemon entry point must be packaged without being named:\n{destinations:#?}"
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

/// Service configuration entirely from the command line — the check that found the missing
/// `--daemon` flag (the fixture's `.nativepkg` carries only metadata, no service
/// settings). The same package was installed and its service run on live Debian, Fedora and Arch.
#[test]
fn service_details_are_taken_entirely_from_the_command_line() {
    let fixture = std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/hello-svc"
    ));
    let out = std::env::temp_dir().join("nativepkg-hello-svc-out");
    let _ = std::fs::remove_dir_all(&out);

    let result = Command::new(binary())
        .current_dir(&fixture)
        .args(["--quiet", "--format", "deb", "--output-dir"])
        .arg(&out)
        .args([
            "--install-dir",
            "/opt/test",
            "--daemon",
            "index.js",
            "--exec-name",
            "hello",
            "--init",
            "systemd",
            "--user",
            "hello",
            "--group",
            "hello",
            "--description",
            "Writes a greeting to its log every second.",
            "index.js",
            "package.json",
        ])
        .output()
        .expect("the binary should run");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let deb = std::fs::read_dir(&out)
        .expect("out")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "deb"))
        .expect("a .deb");
    let package =
        nativepkg_deb::read::parse(&std::fs::read(&deb).expect("bytes")).expect("readable");

    for path in [
        "opt/test/hello-svc/app/index.js",
        "opt/test/hello-svc/bin/hello",
        "usr/bin/hello",
        "usr/lib/systemd/system/hello-svc.service",
    ] {
        assert!(
            package.data.contains_key(path),
            "`{path}` missing:\n{:#?}",
            package.data.keys().collect::<Vec<_>>()
        );
    }
    let unit = package.data_text("usr/lib/systemd/system/hello-svc.service");
    assert!(
        unit.contains("ExecStart=/opt/test/hello-svc/app/index.js"),
        "{unit}"
    );
    assert!(unit.contains("User=hello"), "{unit}");
    let _ = std::fs::remove_dir_all(&out);
}
