//! Metadata comes from a `.nativepkg` file or entirely from flags. `package.json` is never read,
//! so a project needs no npm manifest — and one that is present is ignored.

use std::path::{Path, PathBuf};
use std::process::Command;

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_nativepkg"))
}

fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    #[cfg(not(unix))]
    let _ = path;
}

struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("nativepkg-src-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp project");
        Self { root }
    }

    fn file(&self, name: &str, contents: &str) -> &Self {
        let p = self.root.join(name);
        std::fs::write(&p, contents).expect("write");
        self
    }

    fn entry(&self, name: &str) -> &Self {
        self.file(name, "#!/bin/sh\necho hi\n");
        make_executable(&self.root.join(name));
        self
    }

    fn build(&self, extra: &[&str]) -> nativepkg_deb::read::Package {
        let out = self.root.join("out");
        // `inputs` is a trailing var-arg, so once a positional appears every later token is an
        // input; flags such as `--output-dir` must come before the files in `extra`.
        let output = Command::new(binary())
            .current_dir(&self.root)
            .args(["--quiet", "--format", "deb"])
            .arg("--output-dir")
            .arg(&out)
            .args(extra)
            .output()
            .expect("the binary runs");
        assert!(
            output.status.success(),
            "build failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let path = std::fs::read_dir(&out)
            .expect("output dir")
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "deb"))
            .expect("a .deb was written");
        nativepkg_deb::read::parse(&std::fs::read(path).expect("read")).expect("a readable .deb")
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_dot_nativepkg_file_is_enough_without_a_package_json() {
    let p = Project::new("dotfile");
    p.file(
        ".nativepkg",
        r#"{"package_name":"shelltool","version":"2.0.0","description":"a shell service","maintainer":"Dev <dev@example.com>","init":"none","entrypoints":{"cli":"run.sh"}}"#,
    )
    .entry("run.sh");
    let pkg = p.build(&["run.sh"]);
    assert_eq!(
        pkg.control.get("Package").map(String::as_str),
        Some("shelltool")
    );
    assert_eq!(
        pkg.control.get("Version").map(String::as_str),
        Some("2.0.0")
    );
    assert_eq!(
        pkg.control.get("Maintainer").map(String::as_str),
        Some("Dev <dev@example.com>")
    );
}

#[test]
fn metadata_can_come_entirely_from_flags_with_no_manifest_at_all() {
    let p = Project::new("flags");
    p.entry("tool.sh");
    let pkg = p.build(&[
        "--pkg-name",
        "flagtool",
        "--version",
        "3.1.0",
        "--description",
        "all from flags",
        "--maintainer",
        "F <f@example.com>",
        "--init",
        "none",
        "--cli",
        "tool.sh",
        "tool.sh",
    ]);
    assert_eq!(
        pkg.control.get("Package").map(String::as_str),
        Some("flagtool")
    );
    assert_eq!(
        pkg.control.get("Version").map(String::as_str),
        Some("3.1.0")
    );
}

#[test]
fn a_package_json_is_ignored_even_when_present() {
    let p = Project::new("both");
    // Bogus metadata that must not surface anywhere: package.json is never read.
    p.file(
        "package.json",
        r#"{"name":"from-pkgjson","version":"9.9.9","description":"pkg desc","author":"P <p@example.com>"}"#,
    )
    .file(
        ".nativepkg",
        r#"{"package_name":"from-nativepkg","version":"1.0.0","description":"real","maintainer":"R <r@example.com>","init":"none","entrypoints":{"cli":"index.js"}}"#,
    )
    .file("index.js", "#!/usr/bin/env node\nconsole.log(1)\n");
    make_executable(&p.root.join("index.js"));
    // package.json is passed as an input, but only as a file to ship — never as metadata.
    let pkg = p.build(&["index.js", "package.json"]);
    assert_eq!(
        pkg.control.get("Package").map(String::as_str),
        Some("from-nativepkg")
    );
    assert_eq!(
        pkg.control.get("Version").map(String::as_str),
        Some("1.0.0")
    );
    // nothing from package.json leaked in
    assert_ne!(
        pkg.control.get("Package").map(String::as_str),
        Some("from-pkgjson")
    );
    assert_ne!(
        pkg.control.get("Version").map(String::as_str),
        Some("9.9.9")
    );
}
