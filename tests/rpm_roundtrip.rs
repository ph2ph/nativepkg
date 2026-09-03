//! Build an `.rpm`, parse it back, and assert it says what the plan described. Parsing through
//! the `rpm` crate, not the `rpm` tool, is what makes this run on a Debian host; the real tool,
//! where present, is an extra layer.

use std::path::PathBuf;
use std::process::Command;

use nativepkg::core::arch::Architecture;
use nativepkg::core::plan::{
    BuildPlan, Description, Destination, Identity, PlanMetadata, PlannedFile,
};
use nativepkg::core::timestamp::Timestamp;
use nativepkg::rpm::{Options, build_bytes};

const TIMESTAMP: u64 = 1_700_000_000;

fn dest(path: &str) -> Destination {
    Destination::new(path).expect("fixture path should normalise")
}

fn identity(version: &str, arch: Architecture) -> Identity {
    Identity {
        package_name: "probe-app".into(),
        version_deb: version.into(),
        version_rpm: version.into(),
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

fn plan_with(files: Vec<PlannedFile>, version: &str, arch: Architecture) -> BuildPlan {
    BuildPlan::new(
        identity(version, arch),
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
        ],
        "1.2.3",
        Architecture::Any,
    )
}

fn parse(bytes: &[u8]) -> rpm::Package {
    rpm::Package::parse(&mut std::io::Cursor::new(bytes)).expect("should parse back")
}

fn build(plan: &BuildPlan) -> rpm::Package {
    parse(&build_bytes(plan, &Options::default()).expect("should build"))
}

#[test]
fn the_fixture_exercises_several_entry_kinds() {
    let plan = sample_plan();
    assert!(plan.files.iter().filter(|f| f.is_regular()).count() >= 3);
    assert!(plan.files.iter().any(PlannedFile::is_symlink));
    assert!(!plan.config_files().is_empty());
}

/// Reports rather than fails when `rpmbuild` is installed: the property is that the code needs
/// no toolchain, not that the host lacks one.
#[test]
fn an_rpm_is_produced_without_any_rpm_toolchain() {
    if Command::new("rpmbuild").arg("--version").output().is_ok() {
        eprintln!(
            "NOTE: `rpmbuild` is present on this host, so its absence is not being demonstrated; \
             the build below still never invokes it"
        );
    }
    let bytes = build_bytes(&sample_plan(), &Options::default()).expect("should build");
    assert!(!bytes.is_empty());
    // RPM's lead magic.
    assert_eq!(&bytes[..4], &[0xed, 0xab, 0xee, 0xdb]);
}

#[test]
fn identity_fields_match_the_plan() {
    let package = build(&sample_plan());
    let header = &package.metadata;
    assert_eq!(header.get_name().expect("name"), "probe-app");
    assert_eq!(header.get_version().expect("version"), "1.2.3");
    assert_eq!(header.get_release().expect("release"), "1");
    assert_eq!(header.get_arch().expect("arch"), "noarch");
    assert_eq!(header.get_license().expect("license"), "MIT");
    assert_eq!(header.get_summary().expect("summary"), "a probe");
    assert_eq!(header.get_url().expect("url"), "https://example.com");
}

#[test]
fn architecture_uses_the_rpm_spelling() {
    for (arch, expected) in [
        (Architecture::Any, "noarch"),
        (Architecture::Amd64, "x86_64"),
        (Architecture::Arm64, "aarch64"),
    ] {
        let plan = plan_with(
            vec![PlannedFile::inline(
                dest("/usr/lib/probe-app/app.js"),
                b"x\n".to_vec(),
                PlannedFile::MODE_REGULAR,
            )],
            "1.2.3",
            arch,
        );
        assert_eq!(
            build(&plan).metadata.get_arch().expect("arch"),
            expected,
            "{arch:?}"
        );
    }
}

#[test]
fn the_description_carries_synopsis_and_body() {
    let package = build(&sample_plan());
    let description = package.metadata.get_description().expect("description");
    assert!(description.contains("a probe"), "{description}");
    assert!(description.contains("with a body"), "{description}");
}

#[test]
fn dependencies_become_requirements() {
    let package = build(&sample_plan());
    let names: Vec<String> = package
        .metadata
        .get_requires()
        .expect("requires")
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert!(names.iter().any(|n| n == "nodejs"), "{names:?}");
    assert!(names.iter().any(|n| n == "redis-server"), "{names:?}");
}

#[test]
fn every_planned_file_appears_in_the_payload() {
    let package = build(&sample_plan());
    let paths: Vec<String> = package
        .metadata
        .get_file_paths()
        .expect("paths")
        .into_iter()
        .map(|p| p.display().to_string())
        .collect();

    for expected in [
        "/usr/lib/probe-app/app/app.js",
        "/usr/lib/probe-app/bin/probe-app",
        "/etc/probe-app/config.json",
        "/usr/bin/probe-app",
    ] {
        assert!(
            paths.contains(&expected.to_owned()),
            "{expected} missing from {paths:?}"
        );
    }
}

/// `linkto()` is populated independently of the file type, so asserting it alone passed while
/// every symlink was written as a zero-byte regular file.
#[test]
fn the_symlink_is_stored_as_a_symlink_with_its_planned_target() {
    let package = build(&sample_plan());
    let entry = package
        .metadata
        .get_file_entries()
        .expect("entries")
        .into_iter()
        .find(|e| e.path().display().to_string() == "/usr/bin/probe-app")
        .expect("the link should be present");

    assert_eq!(
        entry.file_type(),
        rpm::FileType::SymbolicLink,
        "a symlink stored as a regular file installs a zero-byte file, not a link"
    );
    assert_eq!(
        entry.linkto(),
        Some("../lib/probe-app/bin/probe-app"),
        "the link must carry the target the plan computed"
    );
}

/// An explicit directory went through a builder method that rejects anything but a regular
/// file, so any plan containing one failed to build; no test constructed one.
#[test]
fn an_explicit_directory_entry_is_packaged() {
    let plan = plan_with(
        vec![
            PlannedFile::inline(
                dest("/usr/lib/probe-app/app.js"),
                b"x\n".to_vec(),
                PlannedFile::MODE_REGULAR,
            ),
            PlannedFile::directory(dest("/var/log/probe-app")),
        ],
        "1.2.3",
        Architecture::Any,
    );
    let package = build(&plan);
    let entry = package
        .metadata
        .get_file_entries()
        .expect("entries")
        .into_iter()
        .find(|e| e.path().display().to_string() == "/var/log/probe-app")
        .expect("an explicitly planned directory must be packaged");
    assert_eq!(entry.file_type(), rpm::FileType::Dir);
    assert_eq!(entry.permissions(), 0o755);
}

/// `noreplace` preserves an administrator's edits across an upgrade, as `conffiles` does.
#[test]
fn configuration_files_are_flagged_and_others_are_not() {
    let package = build(&sample_plan());
    let entries = package.metadata.get_file_entries().expect("entries");

    let config = entries
        .iter()
        .find(|e| e.path().display().to_string() == "/etc/probe-app/config.json")
        .expect("config file present");
    assert!(
        config.flags().contains(rpm::FileFlags::CONFIG),
        "a configuration file must be flagged: {:?}",
        config.flags()
    );
    assert!(
        config.flags().contains(rpm::FileFlags::NOREPLACE),
        "edits must survive an upgrade: {:?}",
        config.flags()
    );

    let ordinary = entries
        .iter()
        .find(|e| e.path().display().to_string() == "/usr/lib/probe-app/app/app.js")
        .expect("ordinary file present");
    assert!(
        !ordinary.flags().contains(rpm::FileFlags::CONFIG),
        "an ordinary file must not be flagged as configuration: {:?}",
        ordinary.flags()
    );
}

#[test]
fn maintainer_scripts_become_scriptlets() {
    let options = Options {
        maintainer_scripts: vec![
            ("post".to_owned(), b"#!/bin/sh\nexit 0\n".to_vec()),
            ("preun".to_owned(), b"#!/bin/sh\nexit 0\n".to_vec()),
        ],
        ..Options::default()
    };
    let package = parse(&build_bytes(&sample_plan(), &options).expect("should build"));
    assert!(package.metadata.get_post_install_script().is_ok());
    assert!(package.metadata.get_pre_uninstall_script().is_ok());
}

#[test]
fn an_unmappable_scriptlet_name_is_refused() {
    let options = Options {
        maintainer_scripts: vec![("triggerin".to_owned(), b"x".to_vec())],
        ..Options::default()
    };
    let err = build_bytes(&sample_plan(), &options).expect_err("RPM has four slots");
    assert!(err.to_string().contains("triggerin"), "{err}");
}

#[test]
fn two_builds_of_one_plan_are_byte_identical() {
    let plan = sample_plan();
    let first = build_bytes(&plan, &Options::default()).expect("build");
    let second = build_bytes(&plan, &Options::default()).expect("build");
    assert_eq!(first, second);
}

#[test]
fn a_different_timestamp_produces_different_bytes() {
    let plan = sample_plan();
    let mut other = sample_plan();
    other.timestamp = Timestamp::from_secs(TIMESTAMP + 1000);
    assert_ne!(
        build_bytes(&plan, &Options::default()).expect("build"),
        build_bytes(&other, &Options::default()).expect("build")
    );
}

#[test]
fn the_build_time_is_the_plans_not_the_clock() {
    let package = build(&sample_plan());
    let build_time = package.metadata.get_build_time().expect("build time");
    assert_eq!(build_time, TIMESTAMP);
}

/// Against the mapping function's own output, not hand-written literals that merely share the
/// convention.
#[test]
fn the_mappers_own_output_sorts_correctly_under_rpm_rules() {
    use core::cmp::Ordering;
    use nativepkg::core::version::{MappedVersion, VersionSpec};

    let mapped = |input: &str| {
        let spec = VersionSpec::parse(input).expect("test input should parse");
        MappedVersion::new(&spec, None).expect("test input should map")
    };

    for (pre, release) in [
        ("1.2.3-beta.1", "1.2.3"),
        ("1.2.3-rc.1", "1.2.3"),
        ("1.2.3-beta.1", "1.2.3-rc.1"),
        ("2.0.0-alpha.1+build.5", "2.0.0"),
    ] {
        let lower = mapped(pre);
        let higher = mapped(release);
        assert_eq!(
            rpm_version::rpm_evr_compare(lower.rpm_version(), higher.rpm_version()),
            Ordering::Less,
            "`{pre}` mapped to `{}` should sort below `{release}` mapped to `{}`",
            lower.rpm_version(),
            higher.rpm_version()
        );
    }
}

/// So the ordering proved above is the one RPM will see.
#[test]
fn the_package_carries_the_mapped_version() {
    use nativepkg::core::version::{MappedVersion, VersionSpec};

    let spec = VersionSpec::parse("1.2.3-beta.1").expect("parses");
    let mapped = MappedVersion::new(&spec, None).expect("maps");
    assert_eq!(mapped.rpm_version(), "1.2.3~beta.1");

    let plan = plan_with(
        vec![PlannedFile::inline(
            dest("/usr/lib/probe-app/app.js"),
            b"x\n".to_vec(),
            PlannedFile::MODE_REGULAR,
        )],
        mapped.rpm_version(),
        Architecture::Any,
    );
    assert_eq!(
        build(&plan).metadata.get_version().expect("version"),
        "1.2.3~beta.1"
    );
}

/// Why the mapping is shared between formats rather than Debian-only.
#[test]
fn the_unmapped_npm_spelling_sorts_the_wrong_way_under_rpm_rules() {
    use core::cmp::Ordering;
    assert_eq!(
        rpm_version::rpm_evr_compare("1.2.3-beta.1", "1.2.3"),
        Ordering::Greater,
        "if this ever fails, the tilde mapping is no longer needed for RPM"
    );
}

#[test]
fn the_rpm_tool_accepts_the_package_when_available() {
    if Command::new("rpm").arg("--version").output().is_err() {
        eprintln!("SKIP: `rpm` not on PATH; the package was verified by parsing it back only");
        return;
    }

    let dir = std::env::temp_dir().join("nativepkg-rpm-tool-check");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path: PathBuf = dir.join("probe.rpm");
    std::fs::write(
        &path,
        build_bytes(&sample_plan(), &Options::default()).expect("build"),
    )
    .expect("write");

    let query = Command::new("rpm")
        .args(["-qip", path.to_str().expect("utf-8 path")])
        .output()
        .expect("rpm was available a moment ago");
    std::fs::remove_file(&path).ok();

    assert!(
        query.status.success(),
        "rpm rejected the package: {}",
        String::from_utf8_lossy(&query.stderr)
    );
}

#[test]
fn an_epoch_is_carried_in_its_own_field() {
    let mut identity = identity("1.2.3", Architecture::Any);
    identity.epoch = Some(2);
    let plan = BuildPlan::new(
        identity,
        vec![PlannedFile::inline(
            dest("/usr/lib/probe-app/app.js"),
            b"x\n".to_vec(),
            PlannedFile::MODE_REGULAR,
        )],
        Timestamp::from_secs(TIMESTAMP),
        PlanMetadata {
            generator: "nativepkg".into(),
            generator_version: "0.1.0".into(),
        },
    )
    .expect("plan should assemble");

    let package = build(&plan);
    assert_eq!(package.metadata.get_epoch().expect("epoch"), 2);
    assert_eq!(
        package.metadata.get_version().expect("version"),
        "1.2.3",
        "the epoch must not be folded into the version string"
    );
}

#[test]
fn no_epoch_means_no_epoch_field() {
    let package = build(&sample_plan());
    assert!(package.metadata.get_epoch().is_err());
}

/// Every other fixture uses inline content, a different code path in the `rpm` crate; the
/// streaming path is what real packages use for everything.
#[test]
fn streamed_files_reach_the_payload() {
    let dir = std::env::temp_dir().join("nativepkg-rpm-streamed");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let source = dir.join("payload.bin");
    let content = vec![7u8; 4096];
    std::fs::write(&source, &content).expect("write");

    let plan = plan_with(
        vec![PlannedFile::from_source(
            dest("/usr/lib/probe-app/payload.bin"),
            source.clone(),
            content.len() as u64,
            false,
        )],
        "1.2.3",
        Architecture::Any,
    );
    let package = build(&plan);
    std::fs::remove_file(&source).ok();

    let entry = package
        .metadata
        .get_file_entries()
        .expect("entries")
        .into_iter()
        .find(|e| e.path().display().to_string() == "/usr/lib/probe-app/payload.bin")
        .expect("the streamed file should be present");
    assert_eq!(entry.size(), content.len());
    assert_eq!(entry.file_type(), rpm::FileType::Regular);
}

/// Pins the limitation documented on `add_entry`, in both directions, so an `rpm` release that
/// closes it is noticed. The crate stores `min(source_date, mtime)`: a source older than the
/// plan keeps its own mtime (the leak); a newer one is clamped to the plan's value, which is
/// what `SOURCE_DATE_EPOCH` with a later checkout produces.
#[test]
fn streamed_file_timestamps_follow_the_documented_rule() {
    for (label, offset, expected) in [
        (
            "older than the plan: leaks",
            -10_000_i64,
            TIMESTAMP - 10_000,
        ),
        ("newer than the plan: clamped", 10_000_i64, TIMESTAMP),
    ] {
        let dir = std::env::temp_dir().join(format!(
            "nativepkg-rpm-mtime-{}",
            if offset < 0 { "older" } else { "newer" }
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let source = dir.join("payload.bin");
        std::fs::write(&source, b"x").expect("write");
        set_mtime(&source, TIMESTAMP.wrapping_add_signed(offset));

        let plan = plan_with(
            vec![PlannedFile::from_source(
                dest("/usr/lib/probe-app/payload.bin"),
                source.clone(),
                1,
                false,
            )],
            "1.2.3",
            Architecture::Any,
        );
        let package = build(&plan);
        std::fs::remove_file(&source).ok();

        // By path, not position: once the package declared its directories the first entry
        // became `/usr/lib/probe-app`, and this asserted a directory's timestamp.
        let entry = package
            .metadata
            .get_file_entries()
            .expect("entries")
            .into_iter()
            .find(|e| e.path().display().to_string() == "/usr/lib/probe-app/payload.bin")
            .expect("the streamed file");
        assert_eq!(
            u64::from(entry.modified_at().0),
            expected,
            "{label}: the crate stores min(source_date, mtime). If the first case now equals \
             the plan's timestamp, the limitation documented on `add_entry` has been fixed \
             upstream and that note should be removed"
        );
    }
}

/// Without pulling in a crate whose only use would be one line.
fn set_mtime(path: &std::path::Path, epoch_seconds: u64) {
    let status = Command::new("touch")
        .arg("-d")
        .arg(format!("@{epoch_seconds}"))
        .arg(path)
        .status()
        .expect("touch should be available");
    assert!(status.success(), "could not set the fixture's mtime");
}

/// An earlier version enumerated system directories literally; review built with
/// `install_dir = /usr/local/lib` and the package claimed `/usr/local` and `/usr/local/lib`,
/// both owned by `filesystem`.
#[test]
fn ownership_scales_with_a_custom_installation_root() {
    let mut plan = sample_plan();
    plan.files = plan
        .files
        .iter()
        .filter(|file| file.destination.as_str().starts_with("/usr/lib/probe-app"))
        .map(|file| {
            let mut moved = file.clone();
            moved.destination = Destination::new(
                file.destination
                    .as_str()
                    .replace("/usr/lib/probe-app", "/usr/local/lib/probe-app"),
            )
            .expect("a valid destination");
            moved
        })
        .collect();

    let bytes = build_bytes(&plan, &Options::default()).expect("builds");
    let package =
        rpm::Package::parse(&mut std::io::Cursor::new(bytes)).expect("a readable package");
    let directories: Vec<String> = package
        .metadata
        .get_file_entries()
        .expect("file entries")
        .iter()
        .filter(|entry| entry.file_type() == rpm::FileType::Dir)
        .map(|entry| entry.path().display().to_string())
        .collect();

    assert!(
        directories.iter().any(|d| d == "/usr/local/lib/probe-app"),
        "the package must own the root it creates:\n{directories:#?}"
    );
    for shared in ["/usr", "/usr/local", "/usr/local/lib"] {
        assert!(
            !directories.iter().any(|d| d == shared),
            "`{shared}` belongs to the base system; a package must not claim it:\n\
             {directories:#?}"
        );
    }
}

/// Without directory entries `rpm` has no record that the package owns `/usr/lib/<name>`, and
/// `rpm --erase` leaves the tree behind as empty directories — four husks on a Fedora
/// container. The other backends computed this independently, so no file-only comparison saw it.
#[test]
fn the_package_owns_the_directories_its_files_live_in() {
    let plan = sample_plan();
    let bytes = build_bytes(&plan, &Options::default()).expect("builds");
    let package =
        rpm::Package::parse(&mut std::io::Cursor::new(bytes)).expect("a readable package");

    let entries = package.metadata.get_file_entries().expect("file entries");

    let directories: Vec<String> = entries
        .iter()
        .filter(|entry| entry.file_type() == rpm::FileType::Dir)
        .map(|entry| entry.path().display().to_string())
        .collect();

    assert!(
        !directories.is_empty(),
        "the package declares no directories at all, so `rpm --erase` would leave every one \
         of them behind"
    );

    // System directories belong to the `filesystem` package; claiming them would make two
    // packages own one path.
    for expected in [
        "/usr/lib/probe-app",
        "/usr/lib/probe-app/app",
        "/usr/lib/probe-app/bin",
    ] {
        assert!(
            directories.iter().any(|d| d == expected),
            "`{expected}` is created by this package but not owned, so `rpm --erase` would \
             leave it behind:\n{directories:#?}"
        );
    }

    for system in ["/usr", "/usr/lib", "/etc", "/var", "/var/log"] {
        assert!(
            !directories.iter().any(|d| d == system),
            "`{system}` belongs to the base system; a package must not claim it:\n\
             {directories:#?}"
        );
    }
}

/// Fedora's preset policy is `disable *`, so the package ships the policy that enables its
/// unit — and leaves the directory to systemd, as it does the unit directory.
#[test]
fn a_preset_service_ships_its_enabling_policy_without_owning_the_directory() {
    let options = Options {
        preset_service: Some("probe-app".to_owned()),
        ..Options::default()
    };
    let package = parse(&build_bytes(&sample_plan(), &options).expect("should build"));
    let paths: Vec<String> = package
        .metadata
        .get_file_paths()
        .expect("paths")
        .into_iter()
        .map(|p| p.display().to_string())
        .collect();
    assert!(
        paths.contains(&"/usr/lib/systemd/system-preset/50-probe-app.preset".to_owned()),
        "{paths:#?}"
    );
    assert!(
        !paths.contains(&"/usr/lib/systemd/system-preset".to_owned()),
        "the preset directory belongs to systemd:\n{paths:#?}"
    );

    let without = parse(&build_bytes(&sample_plan(), &Options::default()).expect("should build"));
    let plain: Vec<String> = without
        .metadata
        .get_file_paths()
        .expect("paths")
        .into_iter()
        .map(|p| p.display().to_string())
        .collect();
    assert!(
        !plain.iter().any(|p| p.contains("system-preset")),
        "no policy is shipped when nothing is preset:\n{plain:#?}"
    );
}
