//! Build an Arch package, read it back, and assert it says what the plan described.
//!
//! Read back with the system `zstd` and `tar`, not the writer's own reader — the method that
//! caught a symlink stored as a regular file in the RPM backend.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::process::{Command, Stdio};

use nativepkg_arch::{Options, build_bytes, mtree, pkginfo};
use nativepkg_core::arch::Architecture;
use nativepkg_core::plan::{
    BuildPlan, Description, Destination, Identity, PlanMetadata, PlannedFile,
};
use nativepkg_core::timestamp::Timestamp;

const TIMESTAMP: u64 = 1_700_000_000;

fn dest(path: &str) -> Destination {
    Destination::new(path).expect("fixture path should normalise")
}

fn identity(arch: Architecture) -> Identity {
    Identity {
        package_name: "probe-app".into(),
        version_deb: "1.2.3".into(),
        version_rpm: "1.2.3".into(),
        release_rpm: "1".into(),
        epoch: None,
        description: Description::split("a probe\n\nwith a body").expect("splits"),
        maintainer: "A <a@example.com>".into(),
        architecture: arch,
        dependencies: Some("nodejs, redis-server".into()),
        homepage: Some("https://example.com".into()),
        license: Some("MIT".into()),
    }
}

fn plan_with(files: Vec<PlannedFile>, arch: Architecture) -> BuildPlan {
    BuildPlan::new(
        identity(arch),
        files,
        Timestamp::from_secs(TIMESTAMP),
        PlanMetadata {
            generator: "nativepkg".into(),
            generator_version: "0.1.0".into(),
        },
    )
    .expect("fixture plan should assemble")
}

fn sample_plan() -> BuildPlan {
    let target = dest("/usr/lib/probe-app/bin/probe-app");
    plan_with(
        vec![
            PlannedFile::inline(
                dest("/usr/lib/probe-app/app/app.js"),
                b"console.log(1)\n".to_vec(),
                PlannedFile::MODE_REGULAR,
            ),
            PlannedFile::inline(
                target.clone(),
                b"#!/bin/sh\nexec node app.js\n".to_vec(),
                PlannedFile::MODE_EXECUTABLE,
            ),
            PlannedFile::inline(
                dest("/etc/probe-app/config.json"),
                b"{}\n".to_vec(),
                PlannedFile::MODE_REGULAR,
            )
            .as_config(),
            PlannedFile::symlink(dest("/usr/bin/probe-app"), &target),
            PlannedFile::directory(dest("/var/log/probe-app")),
        ],
        Architecture::Any,
    )
}

/// Tar entries as `path -> (type char, link target)`, via the system `tar`.
fn listing(bytes: &[u8]) -> BTreeMap<String, (char, Option<String>)> {
    let tar = decompress(bytes);
    let mut child = Command::new("tar")
        .args(["-tvf", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("system tar should be available");
    child.stdin.as_mut().expect("stdin").write_all_bytes(&tar);
    let output = child.wait_with_output().expect("tar should finish");
    assert!(output.status.success(), "system tar rejected the archive");

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let permissions = parts.next()?;
            let rest: Vec<&str> = line.split_whitespace().collect();
            // `tar -tv` prints: perms owner size date time name [-> target]. Index 5 holds
            // because ownership is pinned to `root/root` and no name here has spaces.
            let name_index = 5;
            let name = rest.get(name_index)?.to_string();
            let link = rest
                .iter()
                .position(|p| *p == "->")
                .and_then(|i| rest.get(i + 1))
                .map(ToString::to_string);
            Some((
                name.trim_end_matches('/')
                    .trim_start_matches("./")
                    .to_owned(),
                (permissions.chars().next()?, link),
            ))
        })
        .collect()
}

fn member(bytes: &[u8], name: &str) -> Vec<u8> {
    let tar = decompress(bytes);
    let mut child = Command::new("tar")
        .args(["-xOf", "-", name])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("system tar should be available");
    child.stdin.as_mut().expect("stdin").write_all_bytes(&tar);
    let output = child.wait_with_output().expect("tar should finish");
    assert!(output.status.success(), "member `{name}` not found");
    output.stdout
}

fn decompress(bytes: &[u8]) -> Vec<u8> {
    zstd_decode(bytes)
}

fn zstd_decode(bytes: &[u8]) -> Vec<u8> {
    let mut child = Command::new("zstd")
        .args(["-d", "-c"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("system zstd should be available");
    child.stdin.as_mut().expect("stdin").write_all_bytes(bytes);
    let output = child.wait_with_output().expect("zstd should finish");
    assert!(output.status.success(), "system zstd rejected the archive");
    output.stdout
}

trait WriteAllBytes {
    fn write_all_bytes(&mut self, bytes: &[u8]);
}

impl<T: std::io::Write> WriteAllBytes for T {
    fn write_all_bytes(&mut self, bytes: &[u8]) {
        self.write_all(bytes).expect("pipe should accept the data");
    }
}

fn pkginfo_of(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&member(bytes, ".PKGINFO")).into_owned()
}

fn mtree_of(bytes: &[u8]) -> String {
    let gz = member(bytes, ".MTREE");
    let mut out = String::new();
    flate2::read::GzDecoder::new(&gz[..])
        .read_to_string(&mut out)
        .expect("the manifest should be gzipped");
    out
}

fn build(plan: &BuildPlan) -> Vec<u8> {
    build_bytes(plan, &Options::default()).expect("should build")
}

#[test]
fn the_fixture_exercises_every_entry_kind() {
    let plan = sample_plan();
    assert!(plan.files.iter().filter(|f| f.is_regular()).count() >= 3);
    assert!(plan.files.iter().any(PlannedFile::is_symlink));
    assert!(!plan.config_files().is_empty());
}

#[test]
fn a_package_is_produced_without_makepkg() {
    if Command::new("makepkg").arg("--version").output().is_ok() {
        eprintln!("NOTE: `makepkg` is present; the build below still never invokes it");
    }
    assert!(!build(&sample_plan()).is_empty());
}

#[test]
fn metadata_members_come_before_the_payload() {
    let tar = decompress(&build(&sample_plan()));
    let mut child = Command::new("tar")
        .args(["-tf", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("tar");
    child.stdin.as_mut().expect("stdin").write_all_bytes(&tar);
    let output = child.wait_with_output().expect("tar");
    let names: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(ToOwned::to_owned)
        .collect();

    assert_eq!(names[0], ".MTREE", "{names:?}");
    assert_eq!(names[1], ".PKGINFO", "{names:?}");
    assert!(
        names[2..].iter().all(|n| !n.starts_with('.')),
        "metadata must precede the payload: {names:?}"
    );
}

#[test]
fn package_information_carries_the_required_fields() {
    let info = pkginfo_of(&build(&sample_plan()));
    for key in [
        "pkgname",
        "pkgbase",
        "xdata",
        "pkgver",
        "pkgdesc",
        "builddate",
        "packager",
        "size",
        "arch",
        "license",
    ] {
        assert!(
            !pkginfo::values(&info, key).is_empty(),
            "missing `{key}` in:\n{info}"
        );
    }
    assert_eq!(pkginfo::values(&info, "pkgname"), vec!["probe-app"]);
    assert_eq!(pkginfo::values(&info, "arch"), vec!["any"]);
    assert_eq!(pkginfo::values(&info, "license"), vec!["MIT"]);
    assert_eq!(
        pkginfo::values(&info, "builddate"),
        vec![TIMESTAMP.to_string()]
    );
}

/// The hyphen is `pkgver`'s own separator, which is why a version may not contain one.
#[test]
fn pkgver_joins_version_and_release() {
    let info = pkginfo_of(&build(&sample_plan()));
    assert_eq!(pkginfo::values(&info, "pkgver"), vec!["1.2.3-1"]);
    assert_eq!(pkginfo::pkgver("1.2.3", "1", Some(2)), "2:1.2.3-1");
}

/// Debian's `Installed-Size` is kibibytes; Arch's `size` is bytes.
#[test]
fn size_is_a_byte_count_not_kibibytes() {
    let plan = plan_with(
        vec![PlannedFile::inline(
            dest("/usr/lib/probe-app/big.bin"),
            vec![0_u8; 5000],
            PlannedFile::MODE_REGULAR,
        )],
        Architecture::Any,
    );
    let info = pkginfo_of(&build(&plan));
    assert_eq!(
        pkginfo::values(&info, "size"),
        vec!["5000"],
        "the field is a byte count"
    );
    assert_ne!(
        pkginfo::values(&info, "size"),
        vec![plan.installed_size_kib().to_string()],
        "reusing the Debian kibibyte figure would be wrong"
    );
}

#[test]
fn dependencies_and_backup_paths_are_declared() {
    let info = pkginfo_of(&build(&sample_plan()));
    assert_eq!(
        pkginfo::values(&info, "depend"),
        vec!["nodejs", "redis-server"]
    );
    assert_eq!(
        pkginfo::values(&info, "backup"),
        vec!["etc/probe-app/config.json"],
        "a backup path is recorded without the leading separator"
    );
}

#[test]
fn architecture_uses_the_arch_spelling() {
    for (arch, expected) in [
        (Architecture::Any, "any"),
        (Architecture::Amd64, "x86_64"),
        (Architecture::Arm64, "aarch64"),
    ] {
        let plan = plan_with(
            vec![PlannedFile::inline(
                dest("/usr/lib/probe-app/app.js"),
                b"x\n".to_vec(),
                PlannedFile::MODE_REGULAR,
            )],
            arch,
        );
        assert_eq!(
            pkginfo::values(&pkginfo_of(&build(&plan)), "arch"),
            vec![expected],
            "{arch:?}"
        );
    }
}

/// Through the system `tar`, so a symlink stored as a regular file cannot pass.
#[test]
fn the_symlink_is_a_symlink_at_the_archive_level() {
    let entries = listing(&build(&sample_plan()));
    let (kind, link) = entries
        .get("usr/bin/probe-app")
        .expect("the link should be present");
    assert_eq!(
        *kind, 'l',
        "system tar must see a symlink, not a regular file"
    );
    assert_eq!(link.as_deref(), Some("../lib/probe-app/bin/probe-app"));
}

#[test]
fn an_explicit_directory_is_a_directory_at_the_archive_level() {
    let entries = listing(&build(&sample_plan()));
    let (kind, _) = entries
        .get("var/log/probe-app")
        .expect("the directory should be present");
    assert_eq!(*kind, 'd');
}

#[test]
fn the_manifest_agrees_with_the_payload_in_both_directions() {
    let bytes = build(&sample_plan());
    let manifest = mtree::parse(&mtree_of(&bytes));
    // Only `.MTREE` is excused: it cannot carry its own digest. Filtering every dot-prefixed
    // name would excuse exactly the entries most easily forgotten.
    let payload: BTreeMap<String, (char, Option<String>)> = listing(&bytes)
        .into_iter()
        .filter(|(name, _)| name != ".MTREE")
        .collect();

    for name in payload.keys() {
        assert!(
            manifest.contains_key(&format!("./{name}")),
            "`{name}` is in the payload but not the manifest"
        );
    }
    for path in manifest.keys() {
        let name = path.trim_start_matches("./");
        assert!(
            payload.contains_key(name),
            "`{path}` is in the manifest but not the payload"
        );
    }
}

#[test]
fn the_manifest_does_not_list_itself() {
    let manifest = mtree::parse(&mtree_of(&build(&sample_plan())));
    assert!(!manifest.contains_key("./.MTREE"), "{manifest:?}");
}

#[test]
fn the_manifest_lists_the_package_information_member() {
    let manifest = mtree::parse(&mtree_of(&build(&sample_plan())));
    let entry = manifest
        .get("./.PKGINFO")
        .expect("`.PKGINFO` must be described by the manifest");
    assert_eq!(
        entry.get("type").map(String::as_str),
        Some("file"),
        "{entry:?}"
    );
    assert!(
        entry.contains_key("sha256digest"),
        "the manifest entry must carry a digest: {entry:?}"
    );
}

#[test]
fn the_manifest_lists_the_install_scriptlet_when_there_is_one() {
    let options = nativepkg_arch::Options {
        install_scriptlet: Some(b"post_install() {\n  :\n}\n".to_vec()),
        ..Options::default()
    };
    let bytes = nativepkg_arch::build_bytes(&sample_plan(), &options).expect("build succeeds");

    let manifest = mtree::parse(&mtree_of(&bytes));
    let entry = manifest
        .get("./.INSTALL")
        .expect("`.INSTALL` must be described by the manifest");
    assert!(entry.contains_key("sha256digest"), "{entry:?}");
    assert!(
        listing(&bytes).contains_key(".INSTALL"),
        "and it must actually be in the archive"
    );
}

#[test]
fn manifest_digests_are_the_real_sha256_of_the_content() {
    use sha2::{Digest, Sha256};
    let manifest = mtree::parse(&mtree_of(&build(&sample_plan())));
    let entry = manifest
        .get("./usr/lib/probe-app/app/app.js")
        .expect("entry present");
    let expected = Sha256::digest(b"console.log(1)\n")
        .iter()
        .fold(String::new(), |mut out, b| {
            use core::fmt::Write as _;
            let _ = write!(out, "{b:02x}");
            out
        });
    assert_eq!(
        entry.get("sha256digest").map(String::as_str),
        Some(expected.as_str())
    );
}

#[test]
fn two_builds_of_one_plan_are_byte_identical() {
    let plan = sample_plan();
    assert_eq!(build(&plan), build(&plan));
}

#[test]
fn a_different_timestamp_produces_different_bytes() {
    let plan = sample_plan();
    let mut other = sample_plan();
    other.timestamp = Timestamp::from_secs(TIMESTAMP + 1000);
    assert_ne!(build(&plan), build(&other));
}

#[test]
fn the_install_scriptlet_is_emitted_only_when_supplied() {
    let without = listing(&build(&sample_plan()));
    assert!(!without.contains_key(".INSTALL"), "{without:?}");

    let options = Options {
        install_scriptlet: Some(b"post_install() { :; }\n".to_vec()),
        ..Options::default()
    };
    let bytes = build_bytes(&sample_plan(), &options).expect("should build");
    assert!(listing(&bytes).contains_key(".INSTALL"));
    assert!(String::from_utf8_lossy(&member(&bytes, ".INSTALL")).contains("post_install"));
}

/// A backend sees the core and neither sibling, so one format's quirks cannot leak into another.
#[test]
fn this_backend_depends_only_on_the_core() {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("manifest readable");
    assert!(manifest.contains("nativepkg-core"));
    assert!(!manifest.contains("nativepkg-deb"), "{manifest}");
    assert!(!manifest.contains("nativepkg-rpm"), "{manifest}");
}

/// Arch's `99-default.preset` is `disable *`: presetting a unit with no policy behind it leaves
/// the service running until the next boot and disabled after it.
#[test]
fn a_preset_service_ships_its_enabling_policy() {
    let options = Options {
        preset_service: Some("probe-app".to_owned()),
        ..Options::default()
    };
    let bytes = nativepkg_arch::build_bytes(&sample_plan(), &options).expect("build succeeds");
    let policy = "usr/lib/systemd/system-preset/50-probe-app.preset";
    assert_eq!(
        member(&bytes, policy),
        b"enable probe-app.service\n",
        "the policy must enable exactly this unit"
    );
    let manifest = mtree::parse(&mtree_of(&bytes));
    assert!(
        manifest.contains_key(&format!("./{policy}")),
        "the manifest must list the policy"
    );

    let plain = nativepkg_arch::build_bytes(&sample_plan(), &Options::default()).expect("build");
    assert!(
        !listing(&plain).keys().any(|k| k.contains("system-preset")),
        "no policy is shipped when nothing is preset"
    );
}
