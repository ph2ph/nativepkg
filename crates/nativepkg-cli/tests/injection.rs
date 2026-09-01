//! What a hostile `.nativepkg` or flag can and cannot do. Every value here was, before the checks
//! it exercises, reproduced end to end: a quote in `install_dir` ran a command from `postinst` as
//! root, `../../etc/cron.d/x` as the executable name planted a root-owned file there, and a
//! line break in `maintainer` wrote its own control fields.

use std::path::{Path, PathBuf};
use std::process::Command;

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_nativepkg"))
}

struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str, config: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("nativepkg-injection-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp project");
        std::fs::write(root.join(".nativepkg"), config).expect("config");
        std::fs::write(
            root.join("index.js"),
            "#!/usr/bin/env node\nconsole.log(1)\n",
        )
        .expect("entry");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(
                root.join("index.js"),
                std::fs::Permissions::from_mode(0o755),
            )
            .expect("chmod");
        }
        Self { root }
    }

    /// Returns (success, stderr, output dir).
    fn build(&self, format: &str, extra: &[&str]) -> (bool, String, PathBuf) {
        let out = self.root.join(format!("out-{format}"));
        let result = Command::new(binary())
            .current_dir(&self.root)
            .args(["--quiet", "--format", format, "--daemon", "index.js"])
            .arg("--output-dir")
            .arg(&out)
            .args(extra)
            .args(["index.js"])
            .output()
            .expect("the binary runs");
        (
            result.status.success(),
            String::from_utf8_lossy(&result.stderr).into_owned(),
            out,
        )
    }

    fn deb(out: &Path) -> nativepkg_deb::read::Package {
        let path = std::fs::read_dir(out)
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

const PLAIN: &str =
    r#"{"package_name":"probe","version":"1.0.0","description":"p","maintainer":"A <a@x.y>"}"#;

#[test]
fn a_quote_in_install_dir_is_refused_before_it_reaches_a_root_script() {
    let project = Project::new("install-dir", PLAIN);
    let (ok, stderr, _) = project.build(
        "deb",
        &["--install-dir", "/usr/lib/x'; touch /tmp/PWNED; y='"],
    );
    assert!(!ok, "must not build");
    assert!(
        stderr.contains("install_dir") && stderr.contains('\''),
        "{stderr}"
    );
}

#[test]
fn a_path_in_executable_name_cannot_leave_usr_bin() {
    let project = Project::new("exec-name", PLAIN);
    let (ok, stderr, _) = project.build("deb", &["--exec-name", "../../etc/cron.d/evil"]);
    assert!(!ok, "must not build");
    assert!(stderr.contains("executable_name"), "{stderr}");
}

#[test]
fn a_line_break_in_author_cannot_add_control_fields() {
    let project = Project::new(
        "author",
        r#"{"package_name":"probe","version":"1.0.0","description":"p","maintainer":"A <a@x.y>\nPre-Depends: evil"}"#,
    );
    let (ok, stderr, _) = project.build("deb", &[]);
    assert!(!ok, "must not build");
    assert!(
        stderr.contains("maintainer") && stderr.contains("line break"),
        "{stderr}"
    );
}

#[test]
fn a_line_break_in_homepage_cannot_add_control_fields() {
    let project = Project::new(
        "homepage",
        r#"{"package_name":"probe","version":"1.0.0","description":"p","maintainer":"A <a@x.y>","homepage":"http://x\nEssential: yes"}"#,
    );
    let (ok, stderr, _) = project.build("deb", &[]);
    assert!(!ok, "must not build");
    assert!(stderr.contains("homepage"), "{stderr}");
}

#[test]
fn a_second_description_line_is_a_body_not_a_unit_directive() {
    let project = Project::new(
        "description-unit",
        r#"{"package_name":"probe","version":"1.0.0","description":"p\nExecStartPre=/bin/touch /tmp/PWNED","maintainer":"A <a@x.y>"}"#,
    );
    let (ok, stderr, out) = project.build("deb", &["--init", "systemd"]);
    assert!(ok, "a multi-line description is legitimate: {stderr}");
    let package = Project::deb(&out);
    let unit = package
        .data
        .keys()
        .find(|k| k.ends_with("probe.service"))
        .cloned()
        .expect("the unit is shipped");
    let text = package.data_text(&unit);
    assert!(text.contains("Description=p\n"), "{text}");
    assert!(
        !text.contains("ExecStartPre"),
        "the body leaked into the unit:\n{text}"
    );
}

#[test]
fn shell_metacharacters_in_a_description_are_quoted_in_the_init_script() {
    let project = Project::new(
        "description-sysv",
        r#"{"package_name":"probe","version":"1.0.0","description":"p\" ; touch /tmp/PWNED ; echo \"$x`y`","maintainer":"A <a@x.y>"}"#,
    );
    let (ok, stderr, out) = project.build("deb", &["--init", "sysv"]);
    assert!(ok, "{stderr}");
    let package = Project::deb(&out);
    let script = package
        .data
        .keys()
        .find(|k| k.ends_with("init.d/probe"))
        .cloned()
        .expect("the init script is shipped");
    let text = package.data_text(&script);
    let line = text
        .lines()
        .find(|l| l.starts_with("DESCRIPTION="))
        .expect("the DESCRIPTION line");
    assert_eq!(
        line,
        r#"DESCRIPTION="p\" ; touch /tmp/PWNED ; echo \"\$x\`y\`""#
    );
}

#[test]
fn an_entrypoint_with_a_shell_metacharacter_is_refused() {
    let project = Project::new("entrypoint", PLAIN);
    let (ok, stderr, _) = project.build("deb", &["--cli-entrypoint", "index.js; touch /tmp/PWNED"]);
    assert!(!ok, "must not build");
    assert!(stderr.contains("entrypoint"), "{stderr}");
}
