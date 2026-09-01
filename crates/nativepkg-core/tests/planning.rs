//! Building a plan from a real source tree, on a temporary fixture so exact contents can be
//! asserted — including what the bash implementation got wrong: absolute inputs, traversal,
//! escaping symlinks, and dependency directories smuggled in on the command line.

use std::fs;
use std::path::{Path, PathBuf};

use nativepkg_core::build::plan_at;
use nativepkg_core::npm::InstallStrategy;
use nativepkg_core::plan::{EntryKind, FileContent};
use nativepkg_core::resolve::{Overrides, ResolvedConfig, Warning};
use nativepkg_core::timestamp::Timestamp;
use nativepkg_core::{Manifest, resolve};

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        Self::with_workspaces(name, "")
    }

    /// `workspaces` is a JSON fragment to splice in, e.g. `,"workspaces":["packages/*"]`.
    fn with_workspaces(name: &str, workspaces: &str) -> Self {
        let root = std::env::temp_dir().join(format!("nativepkg-plan-{name}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("fixture root should be creatable");
        let me = Self { root };
        me.write(
            "package.json",
            &format!(
                r#"{{
                "name": "probe-app",
                "version": "1.2.3",
                "description": "a probe\n\nwith a body",
                "author": "A <a@example.com>",
                "homepage": "https://example.com",
                "license": "MIT"{workspaces},
                "nativepkg": {{ "init": "none", "entrypoints": {{ "cli": "app.js" }} }}
            }}"#
            ),
        );
        me
    }

    fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent should be creatable");
        }
        // The wrapper executes the cli entry point directly, so `app.js` must be something the
        // kernel can run. Thirty-two tests wrote it as a bare `x\n` and built packages whose
        // command could never run; only the first line and the mode change, and only for it.
        let contents = if relative == "app.js" && !contents.starts_with("#!") {
            format!("#!/usr/bin/env node\n{contents}")
        } else {
            contents.to_owned()
        };
        fs::write(&path, &contents).expect("fixture file should be writable");
        #[cfg(unix)]
        if relative == "app.js" {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        path
    }

    fn config(&self) -> ResolvedConfig {
        let manifest =
            Manifest::from_path(self.root.join("package.json")).expect("fixture manifest parses");
        resolve(&manifest, &Overrides::default())
            .expect("fixture manifest resolves")
            .0
    }

    fn config_with(&self, overrides: &Overrides) -> ResolvedConfig {
        let manifest =
            Manifest::from_path(self.root.join("package.json")).expect("fixture manifest parses");
        resolve(&manifest, overrides)
            .expect("fixture manifest resolves")
            .0
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn inputs(names: &[&str]) -> Vec<PathBuf> {
    names.iter().map(PathBuf::from).collect()
}

fn ts() -> Timestamp {
    Timestamp::from_secs(1_700_000_000)
}

#[test]
fn application_files_are_planned_under_the_install_root() {
    let fx = Fixture::new("basic");
    fx.write("app.js", "console.log(1)\n");
    fx.write("lib/a.js", "module.exports = 1\n");

    let (plan, _) =
        plan_at(&fx.config(), &fx.root, &inputs(&["app.js", "lib"]), ts()).expect("should plan");

    let destinations: Vec<&str> = plan.files.iter().map(|f| f.destination.as_str()).collect();
    assert!(
        destinations.contains(&"/usr/lib/probe-app/app/app.js"),
        "{destinations:?}"
    );
    assert!(
        destinations.contains(&"/usr/lib/probe-app/app/lib/a.js"),
        "{destinations:?}"
    );
    // package.json is included automatically, as the bash implementation did.
    assert!(
        destinations.contains(&"/usr/lib/probe-app/app/package.json"),
        "{destinations:?}"
    );
}

#[test]
fn planning_creates_nothing_on_disk() {
    let fx = Fixture::new("no-writes");
    fx.write("app.js", "x\n");
    let before = fs::read_dir(&fx.root).unwrap().count();

    let _ = plan_at(&fx.config(), &fx.root, &inputs(&["app.js"]), ts()).expect("should plan");

    assert_eq!(
        fs::read_dir(&fx.root).unwrap().count(),
        before,
        "planning must not create a staging directory or any other artefact"
    );
}

#[test]
fn content_references_sources_rather_than_copying_bytes() {
    let fx = Fixture::new("lazy");
    fx.write("app.js", "console.log(1)\n");
    let (plan, _) =
        plan_at(&fx.config(), &fx.root, &inputs(&["app.js"]), ts()).expect("should plan");

    let entry = plan
        .files
        .iter()
        .find(|f| f.destination.as_str().ends_with("/app.js"))
        .expect("app.js should be planned");
    match &entry.content {
        FileContent::FromPath { path, len } => {
            assert!(path.ends_with("app.js"));
            // `Fixture::write` prefixes the entry point with a shebang line.
            assert_eq!(*len, "#!/usr/bin/env node\nconsole.log(1)\n".len() as u64);
        }
        other => panic!("expected a streamed source, got {other:?}"),
    }
}

#[test]
fn absolute_inputs_are_refused() {
    let fx = Fixture::new("absolute");
    fx.write("app.js", "x\n");
    let err = plan_at(&fx.config(), &fx.root, &inputs(&["/etc/passwd"]), ts())
        .expect_err("absolute inputs must be refused");
    assert!(err.to_string().contains("absolute"), "{err}");
}

#[test]
fn inputs_that_do_not_exist_are_refused() {
    let fx = Fixture::new("missing");
    let err = plan_at(&fx.config(), &fx.root, &inputs(&["nope.js"]), ts())
        .expect_err("missing inputs must be refused");
    assert!(err.to_string().contains("nope.js"), "{err}");
}

#[cfg(unix)]
#[test]
fn symlinks_pointing_outside_the_project_are_refused() {
    let fx = Fixture::new("escaping-symlink");
    fx.write("app.js", "x\n");
    std::os::unix::fs::symlink("/etc/hostname", fx.root.join("escape.js"))
        .expect("symlink should be creatable");

    let err = plan_at(&fx.config(), &fx.root, &inputs(&["escape.js"]), ts())
        .expect_err("a symlink leaving the project must be refused");
    assert!(err.to_string().contains("outside the project"), "{err}");
}

/// A symlink was planned as a *regular file* whose length was that of the link target string,
/// because `WalkDir` yields `symlink_metadata`.
#[cfg(unix)]
#[test]
fn symlinks_are_planned_as_symlinks_not_as_regular_files() {
    let fx = Fixture::new("internal-symlink");
    fx.write("lib/real.js", &"x".repeat(5000));
    std::os::unix::fs::symlink(fx.root.join("lib/real.js"), fx.root.join("lib/alias.js"))
        .expect("symlink should be creatable");

    let (plan, _) = plan_at(&fx.config(), &fx.root, &inputs(&["lib"]), ts()).expect("should plan");

    let alias = plan
        .files
        .iter()
        .find(|f| f.destination.as_str().ends_with("/lib/alias.js"))
        .expect("the symlink should be planned");

    match &alias.kind {
        EntryKind::Symlink { target } => assert_eq!(target, "real.js"),
        other => panic!("a symlink must not be planned as {other:?}"),
    }
    assert_eq!(
        alias.content,
        FileContent::None,
        "a symlink carries no content"
    );
    assert_eq!(alias.mode, 0o777);

    // The 5000-byte target is counted once; the link used to count as a ~40-byte regular file.
    let sizes: Vec<u64> = plan
        .files
        .iter()
        .filter(|f| f.destination.as_str().ends_with("/lib/real.js"))
        .map(|f| f.content.len())
        .collect();
    assert_eq!(sizes, vec![5000]);
}

#[cfg(unix)]
#[test]
fn relative_symlinks_are_preserved_relative() {
    let fx = Fixture::new("relative-symlink");
    fx.write("lib/deep/real.js", "x\n");
    std::os::unix::fs::symlink("deep/real.js", fx.root.join("lib/alias.js"))
        .expect("symlink should be creatable");

    let (plan, _) = plan_at(&fx.config(), &fx.root, &inputs(&["lib"]), ts()).expect("should plan");
    let alias = plan
        .files
        .iter()
        .find(|f| f.destination.as_str().ends_with("/lib/alias.js"))
        .expect("the symlink should be planned");
    match &alias.kind {
        EntryKind::Symlink { target } => assert_eq!(target, "deep/real.js"),
        other => panic!("expected a symlink, got {other:?}"),
    }
}

/// Checked against the real filesystem, not the destination's depth. An earlier version
/// hardcoded `"../".repeat(ups)` against `/etc/passwd` and relied on the fixture sitting one
/// segment below `temp_dir()` — a property of how deep `/tmp` is, which review flipped under a
/// deeper directory. The target is now computed from the link's own location.
#[cfg(unix)]
#[test]
fn relative_symlinks_that_escape_the_package_are_refused() {
    let outside = std::env::temp_dir().join("nativepkg-relative-escape-target");
    let _ = fs::remove_dir_all(&outside);
    fs::create_dir_all(&outside).expect("outside dir");
    fs::write(outside.join("secret.txt"), "SECRET\n").expect("secret");

    // Vary the depth of the link, and compute the ascent to the temporary directory both sit
    // in, so every case points at the same real file.
    for depth in 1..=4 {
        let fx = Fixture::new(&format!("relative-escape-depth-{depth}"));
        fx.write("app.js", "x\n");
        let dirs: Vec<String> = (0..depth).map(|n| format!("d{n}")).collect();
        let link_dir = dirs.join("/");
        fx.write(&format!("{link_dir}/real.js"), "x\n");

        // Ascend out of the link's directory, then out of the fixture root itself.
        let ascent = "../".repeat(depth + 1);
        let target = format!("{ascent}nativepkg-relative-escape-target/secret.txt");
        let link = fx.root.join(&link_dir).join("escape.js");
        std::os::unix::fs::symlink(&target, &link).expect("symlink should be creatable");
        assert!(
            link.exists(),
            "fixture error: `{target}` at depth {depth} did not produce a live link"
        );

        let err = plan_at(&fx.config(), &fx.root, &inputs(&["app.js", &dirs[0]]), ts()).expect_err(
            &format!("a link escaping the package at depth {depth} must be refused"),
        );
        assert!(
            err.to_string().contains("escapes the package"),
            "depth {depth} was rejected for the wrong reason: {err}"
        );
    }

    let _ = fs::remove_dir_all(&outside);
}

/// The escape must be *live*: too few `..` lands on a path that does not exist, a dangling link
/// — genuine npm debris that `Strictness::Tolerate` skips — so asserting refusal there tests
/// the wrong thing. The relative case is computed from the link's own location.
#[cfg(unix)]
#[test]
fn dependency_symlinks_to_live_files_outside_the_project_fail_the_build() {
    let outside = std::env::temp_dir().join("nativepkg-dep-escape-target");
    let _ = fs::remove_dir_all(&outside);
    fs::create_dir_all(&outside).expect("outside dir");
    let secret = outside.join("secret.txt");
    fs::write(&secret, "SECRET\n").expect("secret");

    // The link is three levels inside the project; ascending that plus the root reaches the
    // directory the outside file sits in.
    let relative_to_outside = format!("{}nativepkg-dep-escape-target/secret.txt", "../".repeat(4));

    for (label, target) in [
        ("absolute, system file", "/etc/passwd".to_owned()),
        ("absolute, outside file", secret.display().to_string()),
        ("relative, outside file", relative_to_outside),
    ] {
        let fx = Fixture::new(&format!(
            "dep-live-escape-{}",
            label.replace([' ', ','], "-")
        ));
        fx.write("app.js", "x\n");
        fx.write("node_modules/evil-pkg/index.js", "1\n");
        fs::create_dir_all(fx.root.join("node_modules/evil-pkg/.bin")).expect("dir");
        let link = fx.root.join("node_modules/evil-pkg/.bin/tool");
        std::os::unix::fs::symlink(&target, &link).expect("symlink should be creatable");

        // Only meaningful if the link is actually live; a dangling one is tolerated by design.
        assert!(
            link.exists(),
            "fixture error: `{target}` ({label}) did not produce a live link"
        );

        let overrides = Overrides {
            install_strategy: Some(InstallStrategy::Copy),
            ..Overrides::default()
        };
        let err = plan_at(
            &fx.config_with(&overrides),
            &fx.root,
            &inputs(&["app.js"]),
            ts(),
        )
        .expect_err(&format!(
            "a dependency symlink to `{target}` ({label}) must fail the build"
        ));
        assert!(
            err.to_string().contains("escapes the package"),
            "`{target}` ({label}) was rejected for the wrong reason: {err}"
        );
    }

    let _ = fs::remove_dir_all(&outside);
}

/// A dangling link used to surface as a bare I/O error from `canonicalize`.
#[cfg(unix)]
#[test]
fn dangling_symlinks_in_the_application_tree_say_so() {
    let fx = Fixture::new("dangling-symlink");
    fx.write("lib/real.js", "x\n");
    std::os::unix::fs::symlink(fx.root.join("lib/gone.js"), fx.root.join("lib/dangling.js"))
        .expect("symlink should be creatable");

    let err = plan_at(&fx.config(), &fx.root, &inputs(&["lib"]), ts())
        .expect_err("a dangling link must be refused");
    assert!(err.to_string().contains("dangling"), "{err}");
}

/// Two *different* sources resolving to one destination were silently deduplicated by walk
/// order.
#[test]
fn two_different_sources_claiming_one_destination_is_an_error() {
    let fx = Fixture::new("collision");
    fx.write("app.js", "REAL CONTENT\n");
    // Also runnable, or the executability check refuses this copy before the collision.
    let extra = fx.write(
        "extra/usr/lib/probe-app/app/app.js",
        "#!/usr/bin/env node\nDIFFERENT\n",
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&extra, fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    let overrides = Overrides {
        extra_files: Some("extra".into()),
        ..Overrides::default()
    };
    let err = plan_at(
        &fx.config_with(&overrides),
        &fx.root,
        &inputs(&["app.js"]),
        ts(),
    )
    .expect_err("colliding destinations must not be resolved by walk order");

    let message = err.to_string();
    assert!(message.contains("both install to"), "{message}");
    assert!(
        message.contains("app.js"),
        "the error should name the sources: {message}"
    );
}

#[test]
fn the_same_source_collected_twice_is_not_an_error() {
    let fx = Fixture::new("same-source-twice");
    fx.write("app.js", "x\n");

    let (plan, _) = plan_at(
        &fx.config(),
        &fx.root,
        &inputs(&["app.js", "package.json"]),
        ts(),
    )
    .expect("collecting one file twice from one source must be a no-op");

    let count = plan
        .files
        .iter()
        .filter(|f| f.destination.as_str().ends_with("/app/package.json"))
        .count();
    assert_eq!(count, 1);
}

#[cfg(unix)]
#[test]
fn symlinks_under_extra_files_are_planned_as_symlinks() {
    let fx = Fixture::new("extra-symlink");
    fx.write("app.js", "x\n");
    fx.write("extra/usr/lib/probe-app/real.so", "binary\n");
    std::os::unix::fs::symlink("real.so", fx.root.join("extra/usr/lib/probe-app/link.so"))
        .expect("symlink should be creatable");

    let overrides = Overrides {
        extra_files: Some("extra".into()),
        ..Overrides::default()
    };
    let (plan, _) = plan_at(
        &fx.config_with(&overrides),
        &fx.root,
        &inputs(&["app.js"]),
        ts(),
    )
    .expect("should plan");

    let link = plan
        .files
        .iter()
        .find(|f| f.destination.as_str() == "/usr/lib/probe-app/link.so")
        .expect("the extra-files symlink should be planned, not silently dropped");
    assert!(
        matches!(link.kind, EntryKind::Symlink { .. }),
        "{:?}",
        link.kind
    );
}

#[cfg(unix)]
#[test]
fn dangling_dependency_links_are_tolerated() {
    let fx = Fixture::new("dangling-dep");
    fx.write("app.js", "x\n");
    fx.write("node_modules/pkg/index.js", "1\n");
    fs::create_dir_all(fx.root.join("node_modules/.bin")).expect("dir");
    std::os::unix::fs::symlink("../pkg/missing.js", fx.root.join("node_modules/.bin/gone"))
        .expect("symlink should be creatable");

    let overrides = Overrides {
        install_strategy: Some(InstallStrategy::Copy),
        ..Overrides::default()
    };
    // A dependency tree full of broken `.bin` links is ordinary; it must not fail the build.
    let (plan, _) = plan_at(
        &fx.config_with(&overrides),
        &fx.root,
        &inputs(&["app.js"]),
        ts(),
    )
    .expect("a dangling link inside node_modules must be tolerated");
    assert!(plan.files.iter().any(|f| {
        f.destination
            .as_str()
            .ends_with("/node_modules/pkg/index.js")
    }));
}

#[test]
fn dependencies_are_vendored_under_the_copy_strategy() {
    let fx = Fixture::new("vendor-copy");
    fx.write("app.js", "x\n");
    fx.write("node_modules/left-pad/index.js", "module.exports = 1\n");

    let overrides = Overrides {
        install_strategy: Some(InstallStrategy::Copy),
        ..Overrides::default()
    };
    let (plan, warnings) = plan_at(
        &fx.config_with(&overrides),
        &fx.root,
        &inputs(&["app.js"]),
        ts(),
    )
    .expect("should plan");

    assert!(
        plan.files.iter().any(|f| f
            .destination
            .as_str()
            .ends_with("/app/node_modules/left-pad/index.js")),
        "vendored dependency should be planned"
    );
    assert!(warnings.contains(&Warning::DependenciesMayIncludeDevelopmentPackages));
}

#[test]
fn dependencies_are_excluded_under_the_install_time_strategy() {
    let fx = Fixture::new("vendor-npm");
    fx.write("app.js", "x\n");
    fx.write("node_modules/left-pad/index.js", "module.exports = 1\n");

    let overrides = Overrides {
        install_strategy: Some(InstallStrategy::NpmInstall),
        ..Overrides::default()
    };
    let (plan, warnings) = plan_at(
        &fx.config_with(&overrides),
        &fx.root,
        &inputs(&["app.js"]),
        ts(),
    )
    .expect("should plan");

    assert!(
        !plan
            .files
            .iter()
            .any(|f| f.destination.as_str().contains("node_modules")),
        "no dependency should be packaged under the install-time strategy"
    );
    assert!(
        warnings
            .iter()
            .any(|w| matches!(w, Warning::DependenciesExcluded { .. }))
    );
}

#[test]
fn naming_the_dependency_directory_does_not_smuggle_it_in() {
    let fx = Fixture::new("smuggle");
    fx.write("app.js", "x\n");
    fx.write("node_modules/left-pad/index.js", "module.exports = 1\n");

    let overrides = Overrides {
        install_strategy: Some(InstallStrategy::NpmInstall),
        ..Overrides::default()
    };
    let (plan, warnings) = plan_at(
        &fx.config_with(&overrides),
        &fx.root,
        &inputs(&["app.js", "node_modules"]),
        ts(),
    )
    .expect("should plan");

    assert!(
        !plan
            .files
            .iter()
            .any(|f| f.destination.as_str().contains("node_modules"))
    );
    assert!(
        warnings
            .iter()
            .any(|w| matches!(w, Warning::DependenciesExcluded { .. })),
        "the user should be told the strategy governs inclusion"
    );
}

#[test]
fn each_dependency_file_appears_exactly_once() {
    let fx = Fixture::new("nested-deps");
    fx.write("app.js", "x\n");
    fx.write("node_modules/a/index.js", "1\n");
    fx.write("node_modules/a/node_modules/b/index.js", "2\n");

    let overrides = Overrides {
        install_strategy: Some(InstallStrategy::Copy),
        ..Overrides::default()
    };
    let (plan, _) = plan_at(
        &fx.config_with(&overrides),
        &fx.root,
        &inputs(&["app.js"]),
        ts(),
    )
    .expect("should plan");

    let mut destinations: Vec<&str> = plan.files.iter().map(|f| f.destination.as_str()).collect();
    let total = destinations.len();
    destinations.sort_unstable();
    destinations.dedup();
    assert_eq!(
        destinations.len(),
        total,
        "a file was planned more than once"
    );
}

#[test]
fn plans_are_deterministic() {
    let fx = Fixture::new("deterministic");
    fx.write("app.js", "x\n");
    fx.write("lib/b.js", "2\n");
    fx.write("lib/a.js", "1\n");

    let cfg = fx.config();
    let (first, _) =
        plan_at(&cfg, &fx.root, &inputs(&["app.js", "lib"]), ts()).expect("should plan");
    let (second, _) =
        plan_at(&cfg, &fx.root, &inputs(&["app.js", "lib"]), ts()).expect("should plan");

    assert_eq!(first, second);
    assert_eq!(
        first.to_json().unwrap(),
        second.to_json().unwrap(),
        "serialised plans must be identical"
    );
}

#[test]
fn entries_are_sorted_by_destination() {
    let fx = Fixture::new("sorted");
    fx.write("app.js", "x\n");
    fx.write("lib/z.js", "1\n");
    fx.write("lib/a.js", "1\n");

    let (plan, _) =
        plan_at(&fx.config(), &fx.root, &inputs(&["app.js", "lib"]), ts()).expect("should plan");

    let destinations: Vec<&str> = plan.files.iter().map(|f| f.destination.as_str()).collect();
    let mut sorted = destinations.clone();
    sorted.sort_unstable();
    assert_eq!(destinations, sorted);
}

#[test]
fn every_entry_carries_the_plan_timestamp_and_root_ownership() {
    let fx = Fixture::new("stamps");
    fx.write("app.js", "x\n");
    let (plan, _) =
        plan_at(&fx.config(), &fx.root, &inputs(&["app.js"]), ts()).expect("should plan");

    assert_eq!(plan.timestamp, ts());
    // Ownership is a property of the plan, not of entries: what removes the need for fakeroot.
    assert_eq!(nativepkg_core::plan::BuildPlan::UID, 0);
    assert_eq!(nativepkg_core::plan::BuildPlan::GID, 0);
}

#[cfg(unix)]
#[test]
fn executable_bit_maps_to_mode() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new("modes");
    let script = fx.write("run.sh", "#!/bin/sh\n");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");
    fx.write("plain.txt", "x\n");

    let (plan, _) = plan_at(
        &fx.config(),
        &fx.root,
        &inputs(&["run.sh", "plain.txt"]),
        ts(),
    )
    .expect("should plan");

    let mode_of = |suffix: &str| {
        plan.files
            .iter()
            .find(|f| f.destination.as_str().ends_with(suffix))
            .unwrap_or_else(|| panic!("{suffix} should be planned"))
            .mode
    };
    assert_eq!(mode_of("run.sh"), 0o755);
    assert_eq!(mode_of("plain.txt"), 0o644);
}

#[test]
fn the_executable_symlink_is_relative_within_usr() {
    let fx = Fixture::new("symlink-target");
    fx.write("app.js", "x\n");
    let (plan, _) =
        plan_at(&fx.config(), &fx.root, &inputs(&["app.js"]), ts()).expect("should plan");

    let link = plan
        .files
        .iter()
        .find(|f| f.destination.as_str() == "/usr/bin/probe-app")
        .expect("the /usr/bin entry point should be planned");
    match &link.kind {
        EntryKind::Symlink { target } => {
            assert_eq!(target, "../lib/probe-app/bin/probe-app");
            assert!(
                !target.starts_with('/'),
                "a link within /usr must be relative per Debian policy"
            );
        }
        other => panic!("expected a symlink, got {other:?}"),
    }
}

#[test]
fn extra_files_land_at_the_filesystem_root_and_etc_becomes_config() {
    let fx = Fixture::new("extra");
    fx.write("app.js", "x\n");
    fx.write("extra/etc/probe-app/config.json", "{}\n");
    fx.write("extra/var/lib/probe-app/seed.txt", "seed\n");

    let overrides = Overrides {
        extra_files: Some("extra".into()),
        ..Overrides::default()
    };
    let (plan, _) = plan_at(
        &fx.config_with(&overrides),
        &fx.root,
        &inputs(&["app.js"]),
        ts(),
    )
    .expect("should plan");

    let config_paths: Vec<&str> = plan
        .config_files()
        .iter()
        .map(|f| f.destination.as_str())
        .collect();
    assert_eq!(config_paths, ["/etc/probe-app/config.json"]);
    assert!(
        plan.files
            .iter()
            .any(|f| f.destination.as_str() == "/var/lib/probe-app/seed.txt")
    );
}

/// A symlink in an **intermediate** component of a named input is resolved by the OS while
/// opening the path, so it never becomes a walked entry; only canonicalising the input up
/// front catches it.
#[cfg(unix)]
#[test]
fn a_symlinked_intermediate_component_cannot_smuggle_files_in() {
    let fx = Fixture::new("intermediate-symlink");
    fx.write("app.js", "x\n");

    let outside = std::env::temp_dir().join("nativepkg-intermediate-outside");
    let _ = fs::remove_dir_all(&outside);
    fs::create_dir_all(outside.join("subdir")).expect("outside dir");
    fs::write(outside.join("subdir/secret.txt"), "SECRET\n").expect("secret");
    std::os::unix::fs::symlink(&outside, fx.root.join("vendor")).expect("symlink");

    let result = plan_at(
        &fx.config(),
        &fx.root,
        &inputs(&["app.js", "vendor/subdir"]),
        ts(),
    );
    let _ = fs::remove_dir_all(&outside);

    let err = result.err().unwrap_or_else(|| {
        panic!("a symlinked intermediate component let content outside the project be packaged")
    });
    let message = err.to_string();
    assert!(message.contains("escapes the package"), "{message}");
    assert!(
        message.contains("vendor/subdir"),
        "the error should name what the user typed: {message}"
    );
}

/// A project root reached through a symlink is an ordinary CI layout; comparing a canonical
/// target against a raw root used to refuse its in-project links.
#[cfg(unix)]
#[test]
fn a_symlinked_project_root_does_not_cause_false_refusals() {
    let fx = Fixture::new("symlinked-root-real");
    fx.write("app.js", "x\n");
    fx.write("lib/real.js", "x\n");
    std::os::unix::fs::symlink("real.js", fx.root.join("lib/alias.js")).expect("symlink");

    let via_link = std::env::temp_dir().join("nativepkg-symlinked-root-view");
    let _ = fs::remove_file(&via_link);
    std::os::unix::fs::symlink(&fx.root, &via_link).expect("root symlink");

    let manifest =
        Manifest::from_path(via_link.join("package.json")).expect("manifest through the link");
    let (cfg, _) = resolve(&manifest, &Overrides::default()).expect("resolves");
    let result = plan_at(&cfg, &via_link, &inputs(&["app.js", "lib"]), ts());
    let _ = fs::remove_file(&via_link);

    let (plan, _) = result.expect("a project reached through a symlink must still plan");
    assert!(
        plan.files
            .iter()
            .any(|f| f.destination.as_str().ends_with("/lib/alias.js")),
        "the in-project link should be planned, not refused"
    );
}

/// npm workspaces link sibling packages into `node_modules`; skipping them built a package that
/// failed at runtime with module-not-found. Gated on the manifest's own `workspaces`.
#[cfg(unix)]
#[test]
fn declared_workspace_packages_are_materialised() {
    let fx = Fixture::with_workspaces("workspaces", r#","workspaces":["packages/*"]"#);
    fx.write("app.js", "x\n");
    fx.write("packages/shared/package.json", r#"{"name":"shared"}"#);
    fx.write("packages/shared/lib/index.js", "module.exports = 1\n");
    fx.write(".env", "SECRET_TOKEN=hunter2\n");
    fs::create_dir_all(fx.root.join("node_modules")).expect("node_modules");
    fx.write("node_modules/left-pad.js", "1\n");
    std::os::unix::fs::symlink(
        fx.root.join("packages/shared"),
        fx.root.join("node_modules/shared"),
    )
    .expect("workspace symlink");

    let overrides = Overrides {
        install_strategy: Some(InstallStrategy::Copy),
        ..Overrides::default()
    };
    let (plan, _) = plan_at(
        &fx.config_with(&overrides),
        &fx.root,
        &inputs(&["app.js"]),
        ts(),
    )
    .expect("should plan");

    let destinations: Vec<&str> = plan.files.iter().map(|f| f.destination.as_str()).collect();
    assert!(
        destinations
            .iter()
            .any(|d| d.ends_with("/node_modules/shared/lib/index.js")),
        "the declared workspace package's files must be packaged: {destinations:?}"
    );
    assert!(
        !destinations.iter().any(|d| d.contains(".env")),
        "declaring workspaces must not admit anything else: {destinations:?}"
    );
}

/// The workspace fix widened the dependency boundary to the project root and let any untrusted
/// package reach any project file. A build failure, not a skip: the only legitimate reason for
/// this shape is an undeclared workspace, and the error says so.
#[cfg(unix)]
#[test]
fn a_dependency_cannot_reach_arbitrary_project_files() {
    // A file outside node_modules; the project root itself (it has a package.json, defeating
    // a "looks like a package" heuristic); the same in a project that declares workspaces;
    // and a target outside the project.
    let cases: [(&str, &str, &str); 4] = [
        ("dep-reach-file", "", "../../.env"),
        ("dep-reach-root", "", "../.."),
        (
            "dep-reach-declared",
            r#","workspaces":["packages/*"]"#,
            "../../.env",
        ),
        ("dep-reach-outside", "", "/etc/passwd"),
    ];

    for (name, workspaces, target) in cases {
        let fx = Fixture::with_workspaces(name, workspaces);
        fx.write("app.js", "x\n");
        fx.write(".env", "SECRET_TOKEN=hunter2\n");
        fx.write("packages/shared/package.json", r#"{"name":"shared"}"#);
        fs::create_dir_all(fx.root.join("node_modules/evil-pkg")).expect("node_modules");
        std::os::unix::fs::symlink(target, fx.root.join("node_modules/evil-pkg/config.js"))
            .expect("hostile symlink");

        let overrides = Overrides {
            install_strategy: Some(InstallStrategy::Copy),
            ..Overrides::default()
        };
        let err = plan_at(
            &fx.config_with(&overrides),
            &fx.root,
            &inputs(&["app.js"]),
            ts(),
        )
        .err()
        .unwrap_or_else(|| panic!("a dependency link to `{target}` must fail the build"));

        let message = err.to_string();
        assert!(
            message.contains("workspace") || message.contains("escapes the package"),
            "`{target}` was rejected without an actionable reason: {message}"
        );
    }
}

/// Admitting the root would re-open the reach-through.
#[cfg(unix)]
#[test]
fn a_workspaces_pattern_naming_the_project_root_grants_nothing() {
    let fx = Fixture::with_workspaces("workspaces-root-pattern", r#","workspaces":["*"]"#);
    fx.write("app.js", "x\n");
    fx.write(".env", "SECRET\n");
    fs::create_dir_all(fx.root.join("node_modules")).expect("node_modules");
    std::os::unix::fs::symlink(&fx.root, fx.root.join("node_modules/evil")).expect("symlink");

    let overrides = Overrides {
        install_strategy: Some(InstallStrategy::Copy),
        ..Overrides::default()
    };
    let err = plan_at(
        &fx.config_with(&overrides),
        &fx.root,
        &inputs(&["app.js"]),
        ts(),
    )
    .expect_err("a root-resolving workspaces pattern must not grant reach-through");
    assert!(err.to_string().contains("workspace"), "{err}");
}

/// An earlier version used `node_modules/loop -> project_root`, which the workspace gate
/// rejects before the walk reaches the `visited` set — so it passed without exercising the
/// guard it names. Two mutually-linking declared workspaces do reach it.
#[cfg(unix)]
#[test]
fn symlink_cycles_between_declared_workspaces_terminate() {
    let fx = Fixture::with_workspaces("cycle", r#","workspaces":["packages/*"]"#);
    fx.write("app.js", "x\n");
    fx.write("packages/a/package.json", r#"{"name":"a"}"#);
    fx.write("packages/b/package.json", r#"{"name":"b"}"#);
    fs::create_dir_all(fx.root.join("packages/a/node_modules")).expect("dir");
    fs::create_dir_all(fx.root.join("packages/b/node_modules")).expect("dir");
    fs::create_dir_all(fx.root.join("node_modules")).expect("dir");
    std::os::unix::fs::symlink(
        fx.root.join("packages/b"),
        fx.root.join("packages/a/node_modules/b"),
    )
    .expect("a -> b");
    std::os::unix::fs::symlink(
        fx.root.join("packages/a"),
        fx.root.join("packages/b/node_modules/a"),
    )
    .expect("b -> a");
    std::os::unix::fs::symlink(fx.root.join("packages/a"), fx.root.join("node_modules/a"))
        .expect("entry link");

    let overrides = Overrides {
        install_strategy: Some(InstallStrategy::Copy),
        ..Overrides::default()
    };
    let err = plan_at(
        &fx.config_with(&overrides),
        &fx.root,
        &inputs(&["app.js"]),
        ts(),
    )
    .expect_err("a materialisation cycle must be reported, not recursed into");
    assert!(
        err.to_string().contains("cycle"),
        "the cycle guard should name the problem: {err}"
    );
}

/// Compiled addons in an `Architecture: all` package install everywhere and work in one place;
/// bash did this on every build.
#[cfg(unix)]
#[test]
fn compiled_addons_in_an_architecture_independent_package_are_warned_about() {
    let fx = Fixture::new("native-addons");
    fx.write("app.js", "x\n");
    fs::create_dir_all(fx.root.join("node_modules/bcrypt/build/Release")).expect("dirs");
    fx.write("node_modules/bcrypt/index.js", "1\n");
    fx.write(
        "node_modules/bcrypt/build/Release/bcrypt.node",
        "\x7fELF fake\n",
    );

    let overrides = Overrides {
        install_strategy: Some(InstallStrategy::Copy),
        ..Overrides::default()
    };
    let (_, warnings) = plan_at(
        &fx.config_with(&overrides),
        &fx.root,
        &inputs(&["app.js"]),
        ts(),
    )
    .expect("should plan");

    let warning = warnings
        .iter()
        .find(|w| {
            matches!(
                w,
                Warning::CompiledAddonsInArchitectureIndependentPackage { .. }
            )
        })
        .expect("a compiled addon in an `all` package must be reported");
    assert!(
        warning.to_string().contains("bcrypt.node"),
        "the warning should name the addon: {warning}"
    );
}

/// Without this, the test above could pass because the warning always fires.
#[cfg(unix)]
#[test]
fn compiled_addons_with_an_explicit_architecture_are_not_warned_about() {
    let fx = Fixture::new("native-addons-arch");
    fx.write("app.js", "x\n");
    fs::create_dir_all(fx.root.join("node_modules/bcrypt/build/Release")).expect("dirs");
    fx.write(
        "node_modules/bcrypt/build/Release/bcrypt.node",
        "\x7fELF fake\n",
    );

    let overrides = Overrides {
        install_strategy: Some(InstallStrategy::Copy),
        architecture: Some("amd64".into()),
        ..Overrides::default()
    };
    let (_, warnings) = plan_at(
        &fx.config_with(&overrides),
        &fx.root,
        &inputs(&["app.js"]),
        ts(),
    )
    .expect("should plan");

    assert!(
        !warnings.iter().any(|w| matches!(
            w,
            Warning::CompiledAddonsInArchitectureIndependentPackage { .. }
        )),
        "an explicit architecture makes the package honest: {warnings:?}"
    );
}

#[test]
fn the_install_time_strategy_warns_about_what_it_costs() {
    let fx = Fixture::new("install-time-warning");
    fx.write("app.js", "x\n");

    let overrides = Overrides {
        install_strategy: Some(InstallStrategy::NpmInstall),
        ..Overrides::default()
    };
    let (_, warnings) = plan_at(
        &fx.config_with(&overrides),
        &fx.root,
        &inputs(&["app.js"]),
        ts(),
    )
    .expect("should plan");

    let warning = warnings
        .iter()
        .find(|w| matches!(w, Warning::DependenciesInstalledAtInstallTime))
        .expect("the install-time strategy must be announced");
    let text = warning.to_string();
    assert!(text.contains("network"), "{text}");
    assert!(text.contains("root"), "{text}");
}

#[test]
fn the_default_strategy_does_not_warn_about_install_time_installation() {
    let fx = Fixture::new("default-strategy-quiet");
    fx.write("app.js", "x\n");
    let (_, warnings) =
        plan_at(&fx.config(), &fx.root, &inputs(&["app.js"]), ts()).expect("should plan");
    assert!(
        !warnings
            .iter()
            .any(|w| matches!(w, Warning::DependenciesInstalledAtInstallTime))
    );
}

#[test]
fn description_is_split_into_synopsis_and_body() {
    let fx = Fixture::new("description");
    fx.write("app.js", "x\n");
    let (plan, _) =
        plan_at(&fx.config(), &fx.root, &inputs(&["app.js"]), ts()).expect("should plan");

    assert_eq!(plan.identity.description.synopsis, "a probe");
    assert_eq!(plan.identity.description.body, "with a body");
}

#[test]
fn identity_carries_every_format_s_version_spelling() {
    let fx = Fixture::new("identity");
    fx.write("app.js", "x\n");
    let (plan, _) =
        plan_at(&fx.config(), &fx.root, &inputs(&["app.js"]), ts()).expect("should plan");

    assert_eq!(plan.identity.version_deb, "1.2.3");
    assert_eq!(plan.identity.version_rpm, "1.2.3");
    assert_eq!(plan.identity.release_rpm, "1");
    assert_eq!(
        plan.identity.homepage.as_deref(),
        Some("https://example.com")
    );
    assert_eq!(plan.identity.license.as_deref(), Some("MIT"));
    assert!(plan.identity.architecture.is_any());
}

#[test]
fn installed_size_is_reported() {
    let fx = Fixture::new("size");
    fx.write("app.js", &"x".repeat(4096));
    let (plan, _) =
        plan_at(&fx.config(), &fx.root, &inputs(&["app.js"]), ts()).expect("should plan");
    assert!(
        plan.installed_size_kib() >= 4,
        "{}",
        plan.installed_size_kib()
    );
}

#[test]
fn long_paths_survive_planning() {
    let fx = Fixture::new("long-paths");
    fx.write("app.js", "x\n");
    let deep = format!("lib/{}/index.js", vec!["nested"; 20].join("/"));
    fx.write(&deep, "x\n");

    let (plan, _) =
        plan_at(&fx.config(), &fx.root, &inputs(&["app.js", "lib"]), ts()).expect("should plan");

    let longest = plan
        .files
        .iter()
        .map(|f| f.destination.as_str().len())
        .max()
        .unwrap_or(0);
    assert!(
        longest > 100,
        "fixture should exercise long paths, got {longest}"
    );
}

#[test]
fn plans_round_trip_through_json() {
    let fx = Fixture::new("json");
    fx.write("app.js", "x\n");
    let (plan, _) =
        plan_at(&fx.config(), &fx.root, &inputs(&["app.js"]), ts()).expect("should plan");

    let json = plan.to_json().expect("plan should serialise");
    let back: nativepkg_core::plan::BuildPlan =
        serde_json::from_str(&json).expect("plan should deserialise");
    assert_eq!(back, plan);
}

/// A stale fixture directory from a previous run would silently change results.
#[test]
fn fixtures_clean_up() {
    let root = {
        let fx = Fixture::new("cleanup");
        fx.write("app.js", "x\n");
        fx.root.clone()
    };
    assert!(
        !Path::new(&root).exists(),
        "fixture directory should be removed"
    );
}

/// A checkout under a permissive umask marks every file `775`, and that went straight into the
/// archive: `package.json` shipped as a program, and lintian reported
/// `executable-not-elf-or-script`.
#[cfg(unix)]
#[test]
fn only_files_that_can_run_keep_the_executable_bit() {
    use std::os::unix::fs::PermissionsExt as _;

    let fx = Fixture::new("shebang-gate");
    let mut paths = vec![fx.write("app.js", "#!/usr/bin/env node\nconsole.log(1);\n")];
    paths.push(fx.write("lib/module.js", "module.exports = 1;\n"));
    paths.push(fx.write("lib/data.json", "{}\n"));
    paths.push(fx.root.join("package.json"));
    for path in &paths {
        fs::set_permissions(path, fs::Permissions::from_mode(0o775)).expect("chmod");
    }

    let (plan, _) = plan_at(
        &fx.config(),
        &fx.root,
        &inputs(&["app.js", "lib", "package.json"]),
        ts(),
    )
    .expect("should plan");
    let mode_of = |suffix: &str| {
        plan.files
            .iter()
            .find(|f| f.destination.as_str().ends_with(suffix))
            .map_or_else(|| panic!("{suffix} should be planned"), |f| f.mode)
    };

    assert_eq!(
        mode_of("/app.js"),
        0o755,
        "a shebang script stays executable"
    );
    assert_eq!(
        mode_of("/module.js"),
        0o644,
        "a module without a shebang is data"
    );
    assert_eq!(mode_of("/data.json"), 0o644);
    assert_eq!(mode_of("/package.json"), 0o644);
}

/// The unit runs it directly and systemd has no ENOEXEC fallback, so a shebang-less entry point
/// installs cleanly and never starts. Refused at planning, the earliest point that knows both
/// the path and its first bytes.
#[cfg(unix)]
#[test]
fn a_daemon_entrypoint_without_a_shebang_is_refused() {
    use std::os::unix::fs::PermissionsExt as _;

    let fx = Fixture::new("entrypoint-no-shebang");
    // Give the fixture a service so the check applies, and a plain-module entry point.
    fx.write(
        "package.json",
        r#"{"name":"probe-app","version":"1.2.3","description":"d","author":"A <a@example.com>",
            "nativepkg":{"init":"systemd","entrypoints":{"daemon":"app.js"}}}"#,
    );
    // Bypasses `Fixture::write`, which gives `app.js` a shebang; this test checks the opposite.
    let app = fx.root.join("app.js");
    fs::write(&app, "module.exports = 1;\n").expect("write");
    fs::set_permissions(&app, fs::Permissions::from_mode(0o755)).expect("chmod");

    let err = plan_at(&fx.config(), &fx.root, &inputs(&["app.js"]), ts())
        .expect_err("an entry point the kernel cannot exec must be refused");
    let msg = err.to_string();
    assert!(msg.contains("app.js"), "{msg}");
    assert!(
        msg.contains("#!/usr/bin/env node"),
        "the message says how to fix it: {msg}"
    );
}

/// The first version `continue`d past every symlink. Review built `app.js -> real.js` with a
/// shebang-less `real.js`: it shipped, and running it gave `Permission denied`. A link passed
/// *as* an input is a different case, below.
#[cfg(unix)]
#[test]
fn a_symlinked_entrypoint_is_checked_through_its_target() {
    use std::os::unix::fs::PermissionsExt as _;

    let fx = Fixture::new("entrypoint-symlink");
    fx.write(
        "package.json",
        r#"{"name":"probe-app","version":"1.2.3","description":"d","author":"A <a@example.com>",
            "nativepkg":{"init":"systemd","entrypoints":{"daemon":"lib/app.js"}}}"#,
    );
    let real = fx.write("lib/real.js", "module.exports = 1;\n");
    fs::set_permissions(&real, fs::Permissions::from_mode(0o755)).expect("chmod");
    std::os::unix::fs::symlink("real.js", fx.root.join("lib/app.js")).expect("symlink");

    let err = plan_at(&fx.config(), &fx.root, &inputs(&["lib"]), ts())
        .expect_err("the target cannot be executed, so the link cannot either");
    assert!(
        err.to_string().contains("real.js"),
        "names the target: {err}"
    );
}

/// An input symlink is canonicalised before the walk, so the package receives the target under
/// its own name and the entry point's name is never planned; `plan` refuses that, or a wrapper
/// would name a file the package does not contain.
#[cfg(unix)]
#[test]
fn a_symlink_passed_as_an_input_cannot_silently_rename_the_entrypoint() {
    use nativepkg_core::build::plan;
    use std::os::unix::fs::PermissionsExt as _;

    let fx = Fixture::new("entrypoint-input-symlink");
    let real = fx.write("real.js", "#!/usr/bin/env node\nconsole.log(1);\n");
    fs::set_permissions(&real, fs::Permissions::from_mode(0o755)).expect("chmod");
    std::os::unix::fs::symlink("real.js", fx.root.join("app.js")).expect("symlink");

    let err = plan(&fx.config(), &fx.root, &inputs(&["app.js", "real.js"]))
        .expect_err("the entry point named by the wrapper is not in the package");
    let msg = err.to_string();
    assert!(
        msg.contains("app.js") && msg.contains("not in the package"),
        "{msg}"
    );
}

/// The wrapper runs the cli entry point directly, so `init: none` is not exempt — the doc
/// comment that said so was wrong.
#[cfg(unix)]
#[test]
fn a_cli_entrypoint_without_a_shebang_is_refused_even_with_no_service() {
    use std::os::unix::fs::PermissionsExt as _;

    let fx = Fixture::new("cli-entrypoint-no-shebang");
    let cli = fx.root.join("app.js");
    fs::write(&cli, "module.exports = 1;\n").expect("write");
    fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).expect("chmod");

    let err = plan_at(&fx.config(), &fx.root, &inputs(&["app.js"]), ts())
        .expect_err("the wrapper executes this file directly");
    let msg = err.to_string();
    assert!(msg.contains("app.js") && msg.contains("wrapper"), "{msg}");
}

#[cfg(unix)]
#[test]
fn a_broken_cli_entrypoint_is_refused_when_the_daemon_one_is_fine() {
    use std::os::unix::fs::PermissionsExt as _;

    let fx = Fixture::new("cli-broken-daemon-fine");
    fx.write(
        "package.json",
        r#"{"name":"probe-app","version":"1.2.3","description":"d","author":"A <a@example.com>",
            "nativepkg":{"init":"systemd","entrypoints":{"daemon":"app.js","cli":"cli.js"}}}"#,
    );
    let app = fx.write("app.js", "#!/usr/bin/env node\nconsole.log(1);\n");
    let cli = fx.write("cli.js", "console.log(2);\n");
    for p in [&app, &cli] {
        fs::set_permissions(p, fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    let err = plan_at(&fx.config(), &fx.root, &inputs(&["app.js", "cli.js"]), ts())
        .expect_err("the cli entry point cannot be executed");
    assert!(err.to_string().contains("cli.js"), "{err}");
}

/// The executed-destination strings were built with a bare `format!` while planned
/// destinations are normalised, so `.../app/./app.js` never equalled `.../app/app.js` and a
/// good package was refused.
#[cfg(unix)]
#[test]
fn an_entrypoint_spelled_with_a_leading_dot_slash_is_accepted() {
    use nativepkg_core::build::plan;

    let fx = Fixture::new("dot-slash-entrypoint");
    fx.write(
        "package.json",
        r#"{"name":"probe-app","version":"1.2.3","description":"d","author":"A <a@example.com>",
            "nativepkg":{"init":"none","entrypoints":{"cli":"./app.js"}}}"#,
    );
    fx.write("app.js", "console.log(1);\n");

    let (built, _, _) = plan(&fx.config(), &fx.root, &inputs(&["app.js"]))
        .expect("`./app.js` and `app.js` are the same file");
    assert!(
        built
            .files
            .iter()
            .any(|f| f.destination.as_str() == "/usr/lib/probe-app/app/app.js"),
        "the entry point is planned under its normalised destination"
    );
}

/// Not a test: what one `BuildPlan::new` costs at `node_modules` scale. Run with
/// `cargo test --release -p nativepkg-core --test planning -- --ignored rebuild_cost --nocapture`.
#[test]
#[ignore = "a measurement, run by hand"]
fn rebuild_cost_at_fifty_thousand_entries() {
    use nativepkg_core::plan::{Destination, PlannedFile};
    use std::time::Instant;

    let mut files = Vec::with_capacity(50_000);
    for dir in 0..2_000 {
        for file in 0..25 {
            files.push(PlannedFile::inline(
                Destination::new(format!(
                    "/usr/lib/probe/app/node_modules/pkg-{dir}/lib/sub/file-{file}.js"
                ))
                .expect("valid"),
                vec![b'x'; 40],
                PlannedFile::MODE_REGULAR,
            ));
        }
    }
    let fx = Fixture::new("rebuild-cost");
    fx.write(
        "package.json",
        r#"{"name":"probe-app","version":"1.2.3","description":"d","author":"A <a@example.com>",
            "nativepkg":{"init":"none","entrypoints":{"cli":"./app.js"}}}"#,
    );
    fx.write("app.js", "console.log(1);\n");
    let (small, _, _) =
        nativepkg_core::build::plan(&fx.config(), &fx.root, &inputs(&["app.js"])).expect("plan");
    files.extend(small.files.iter().cloned());
    let big = nativepkg_core::plan::BuildPlan::new(
        small.identity.clone(),
        files,
        small.timestamp,
        small.metadata.clone(),
    )
    .expect("plan");

    let started = Instant::now();
    let rebuilt = nativepkg_core::plan::BuildPlan::new(
        big.identity.clone(),
        big.files.clone(),
        big.timestamp,
        big.metadata.clone(),
    )
    .expect("rebuild");
    let one = started.elapsed();
    eprintln!(
        "entries={} (with ancestors) one clone+rebuild={one:?}",
        rebuilt.files.len()
    );
}
