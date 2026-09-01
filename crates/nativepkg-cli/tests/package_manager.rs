//! The install-at-unpack strategy runs one command in the generated postinst: plain npm by
//! default, or whatever `--install-command` names. There is no package-manager detection, and no
//! lock file is shipped unless the project lists it as an input.

use std::path::PathBuf;
use std::process::Command;

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_nativepkg"))
}

fn make_executable(path: &std::path::Path) {
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
        let root = std::env::temp_dir().join(format!("nativepkg-pm-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp project");
        std::fs::write(
            root.join(".nativepkg"),
            r#"{"package_name":"svc","version":"1.0.0","description":"d","maintainer":"A <a@x.y>"}"#,
        )
        .expect("config");
        std::fs::write(
            root.join("index.js"),
            "#!/usr/bin/env node\nconsole.log(1)\n",
        )
        .expect("entry");
        make_executable(&root.join("index.js"));
        Self { root }
    }

    fn file(&self, name: &str, contents: &str) -> &Self {
        std::fs::write(self.root.join(name), contents).expect("write");
        self
    }

    fn build(&self, extra: &[&str]) -> nativepkg_deb::read::Package {
        let out = self.root.join("out");
        let status = Command::new(binary())
            .current_dir(&self.root)
            .args([
                "--quiet",
                "--format",
                "deb",
                "--install-dir",
                "/opt/test",
                "--daemon",
                "index.js",
                "--exec-name",
                "svc",
                "--init",
                "none",
                "--install-strategy",
                "npm-install",
            ])
            .arg("--output-dir")
            .arg(&out)
            .args(extra)
            .args(["index.js"])
            .status()
            .expect("the binary runs");
        assert!(status.success(), "build failed");
        let path = std::fs::read_dir(&out)
            .expect("output dir")
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "deb"))
            .expect("a .deb");
        nativepkg_deb::read::parse(&std::fs::read(path).expect("read")).expect("a readable .deb")
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn postinst(pkg: &nativepkg_deb::read::Package) -> String {
    pkg.script_bodies
        .get("postinst")
        .cloned()
        .expect("a postinst is generated")
}

#[test]
fn npm_install_uses_plain_npm_by_default() {
    let post = postinst(&Project::new("default").build(&[]));
    assert!(post.contains("command -v npm"), "{post}");
    assert!(
        post.contains("npm install --omit=dev --ignore-scripts --no-audit --no-fund"),
        "{post}"
    );
}

#[test]
fn a_custom_install_command_is_used_verbatim_and_guards_its_own_binary() {
    let post = postinst(
        &Project::new("custom")
            .build(&["--install-command", "pnpm install --prod --frozen-lockfile"]),
    );
    assert!(
        post.contains("pnpm install --prod --frozen-lockfile"),
        "{post}"
    );
    // the guard binary is derived from the command's first word
    assert!(post.contains("command -v pnpm"), "{post}");
    assert!(
        !post.contains("command -v npm"),
        "npm should not be the guard: {post}"
    );
}

#[test]
fn a_lock_file_is_not_shipped_unless_it_is_named_as_an_input() {
    let pkg = Project::new("no-auto-lock")
        .file("pnpm-lock.yaml", "lock\n")
        .build(&[]);
    assert!(
        !pkg.data.keys().any(|k| k.ends_with("pnpm-lock.yaml")),
        "a lock file must not be shipped automatically: {:?}",
        pkg.data.keys().collect::<Vec<_>>()
    );
}
