//! Build a `.deb`, parse it back, and assert it says what the plan described. Round-tripping
//! through this crate's own reader is what makes the suite run on any host; `dpkg-deb`, where
//! present, is an extra layer, never a prerequisite.

use std::path::PathBuf;
use std::process::Command;

use nativepkg::core::arch::Architecture;
use nativepkg::core::plan::{
    BuildPlan, Description, Destination, Identity, PlanMetadata, PlannedFile,
};
use nativepkg::core::timestamp::Timestamp;
use nativepkg::deb::read::EntryKind;
use nativepkg::deb::{Compression, Options, build_bytes, read};

const TIMESTAMP: u64 = 1_700_000_000;

fn dest(path: &str) -> Destination {
    Destination::new(path).expect("fixture path should normalise")
}

fn plan_with_files(files: Vec<PlannedFile>, description: &str) -> BuildPlan {
    let identity = Identity {
        package_name: "probe-app".into(),
        version_deb: "1.2.3".into(),
        version_rpm: "1.2.3".into(),
        release_rpm: "1".into(),
        epoch: None,
        description: Description::split(description).expect("description should split"),
        maintainer: "A <a@example.com>".into(),
        architecture: Architecture::Any,
        dependencies: Some("nodejs".into()),
        homepage: Some("https://example.com".into()),
        license: Some("MIT".into()),
    };
    BuildPlan::new(
        identity,
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
    plan_with_files(
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
        "a probe\n\nwith a body",
    )
}

fn build(plan: &BuildPlan, options: &Options) -> read::Package {
    let bytes = build_bytes(plan, options).expect("should build");
    read::parse(&bytes).expect("should parse back")
}

#[test]
fn the_fixture_actually_exercises_several_entry_kinds() {
    let plan = sample_plan();
    assert!(
        plan.files.iter().filter(|f| f.is_regular()).count() >= 3,
        "fixture should contain several regular files"
    );
    assert!(plan.files.iter().any(PlannedFile::is_symlink));
    assert!(!plan.config_files().is_empty());
}

#[test]
fn the_container_has_the_three_members_in_order() {
    let package = build(&sample_plan(), &Options::default());
    assert_eq!(
        package.members,
        vec!["debian-binary", "control.tar.xz", "data.tar.xz"]
    );
}

#[test]
fn every_compressor_produces_a_readable_package() {
    for compression in [
        Compression::Gzip,
        Compression::Xz,
        Compression::Zstd,
        Compression::None,
    ] {
        let options = Options {
            compression,
            ..Options::default()
        };
        let package = build(&sample_plan(), &options);
        assert_eq!(
            package.members[1],
            compression.member_name("control"),
            "{compression:?}"
        );
        assert!(
            package.data.contains_key("usr/lib/probe-app/app/app.js"),
            "{compression:?} lost the payload"
        );
    }
}

#[test]
fn control_fields_match_the_plan() {
    let package = build(&sample_plan(), &Options::default());
    for (field, expected) in [
        ("Package", "probe-app"),
        ("Version", "1.2.3"),
        ("Architecture", "all"),
        ("Depends", "nodejs"),
        ("Homepage", "https://example.com"),
    ] {
        assert_eq!(
            package.control.get(field).map(String::as_str),
            Some(expected),
            "field `{field}`"
        );
    }
    for field in ["Installed-Size", "Priority", "Section", "Maintainer"] {
        assert!(package.control.contains_key(field), "missing `{field}`");
    }
}

/// The bash implementation aborted on this input, or produced a control file no parser could
/// read.
#[test]
fn a_multi_line_description_survives_a_round_trip() {
    let package = build(&sample_plan(), &Options::default());
    let description = package.control.get("Description").expect("present");
    assert!(description.starts_with("a probe"), "{description}");
    assert!(description.contains("with a body"), "{description}");
}

#[test]
fn parent_directories_are_present_exactly_once() {
    let package = build(&sample_plan(), &Options::default());
    for directory in [
        "usr",
        "usr/lib",
        "usr/lib/probe-app",
        "etc",
        "etc/probe-app",
    ] {
        let entry = package
            .data
            .get(directory)
            .unwrap_or_else(|| panic!("directory `{directory}` missing; lintian requires it"));
        assert_eq!(entry.kind, EntryKind::Directory, "{directory}");
        // Counted against the raw stream: a `BTreeMap` collapses duplicates on insert, so it is
        // blind to how many times an entry appears. Removing the writer's deduplication used
        // to leave this green.
        assert_eq!(
            package.occurrences(directory),
            1,
            "`{directory}` appears {} times in data.tar",
            package.occurrences(directory)
        );
    }
}

/// Only ancestors were emitted, so an explicitly planned `/var/log/<package>` vanished with no
/// error and no size discrepancy; the mode was then dropped separately. The non-default mode is
/// a struct literal because `PlannedFile::directory` hardcodes `0o755`, which could not fail.
#[cfg(unix)]
#[test]
fn an_explicit_directory_entry_is_packaged_with_its_own_mode() {
    let restricted = PlannedFile {
        destination: dest("/var/log/probe-app"),
        kind: nativepkg::core::plan::EntryKind::Directory,
        content: nativepkg::core::plan::FileContent::None,
        mode: 0o700,
        is_config: false,
    };
    let plan = plan_with_files(
        vec![
            PlannedFile::inline(
                dest("/usr/lib/probe-app/app.js"),
                b"x\n".to_vec(),
                PlannedFile::MODE_REGULAR,
            ),
            restricted,
        ],
        "a probe",
    );
    let package = build(&plan, &Options::default());

    let entry = package
        .data
        .get("var/log/probe-app")
        .expect("an explicitly planned directory must be packaged");
    assert_eq!(entry.kind, EntryKind::Directory);
    assert_eq!(
        entry.mode, 0o700,
        "the mode the plan asked for must reach the archive"
    );
    assert_eq!(package.occurrences("var/log/probe-app"), 1);

    // An ancestor nobody asked for explicitly keeps the conventional mode.
    assert_eq!(package.data["var/log"].mode, 0o755);
}

#[test]
fn a_directory_that_is_also_an_ancestor_is_not_duplicated() {
    let plan = plan_with_files(
        vec![
            PlannedFile::directory(dest("/usr/lib/probe-app")),
            PlannedFile::inline(
                dest("/usr/lib/probe-app/app.js"),
                b"x\n".to_vec(),
                PlannedFile::MODE_REGULAR,
            ),
        ],
        "a probe",
    );
    let package = build(&plan, &Options::default());
    assert_eq!(package.occurrences("usr/lib/probe-app"), 1);
}

#[test]
fn entry_modes_and_ownership_follow_their_kind() {
    let package = build(&sample_plan(), &Options::default());
    for (path, entry) in &package.data {
        assert_eq!(entry.uid, 0, "`{path}` must be owned by root");
        assert_eq!(entry.gid, 0, "`{path}` must be owned by root");
    }
    assert_eq!(package.data["usr/lib/probe-app"].mode, 0o755, "directory");
    assert_eq!(
        package.data["usr/lib/probe-app/bin/probe-app"].mode, 0o755,
        "executable"
    );
    assert_eq!(
        package.data["usr/lib/probe-app/app/app.js"].mode, 0o644,
        "regular file"
    );
    assert_eq!(package.data["usr/bin/probe-app"].mode, 0o777, "symlink");
    assert_eq!(
        package.data["etc/probe-app/config.json"].mode, 0o644,
        "configuration file"
    );
}

#[test]
fn the_symlink_survives_with_its_planned_target() {
    let package = build(&sample_plan(), &Options::default());
    let link = &package.data["usr/bin/probe-app"];
    assert_eq!(link.kind, EntryKind::Symlink);
    assert_eq!(
        link.link_target.as_deref(),
        Some("../lib/probe-app/bin/probe-app"),
        "a link within /usr must stay relative"
    );
}

#[test]
fn md5sums_cover_regular_files_and_exclude_symlinks() {
    let package = build(&sample_plan(), &Options::default());
    assert!(package.md5sums.contains_key("usr/lib/probe-app/app/app.js"));
    assert!(
        !package.md5sums.contains_key("usr/bin/probe-app"),
        "a symlink has no content to checksum"
    );
    assert!(
        !package.md5sums.keys().any(|k| k.starts_with('/')),
        "md5sums paths carry no leading separator"
    );
}

/// Of the content, not of something adjacent such as the path or the length.
#[test]
fn md5sums_are_the_real_digests_of_the_content() {
    let content = b"console.log(1)\n";
    let expected = md5_hex(content);
    let package = build(&sample_plan(), &Options::default());
    assert_eq!(package.md5sums["usr/lib/probe-app/app/app.js"], expected);
}

#[test]
fn configuration_files_are_declared_and_nothing_else_is() {
    let package = build(&sample_plan(), &Options::default());
    assert_eq!(package.conffiles, vec!["/etc/probe-app/config.json"]);
}

#[test]
fn a_package_with_no_configuration_has_no_conffiles_member() {
    let plan = plan_with_files(
        vec![PlannedFile::inline(
            dest("/usr/lib/probe-app/app.js"),
            b"x\n".to_vec(),
            PlannedFile::MODE_REGULAR,
        )],
        "a probe",
    );
    assert!(build(&plan, &Options::default()).conffiles.is_empty());
}

#[test]
fn policy_required_documentation_is_shipped_and_accounted_for() {
    let package = build(&sample_plan(), &Options::default());
    assert!(
        package
            .data
            .contains_key("usr/share/doc/probe-app/copyright"),
        "a missing copyright file is a lintian error"
    );
    assert!(
        package
            .data
            .contains_key("usr/share/doc/probe-app/changelog.gz"),
        "a missing changelog is a lintian error"
    );
    assert!(
        package
            .md5sums
            .contains_key("usr/share/doc/probe-app/copyright"),
        "documentation must be checksummed like any other file"
    );
}

/// The bash implementation emitted no `Installed-Size`, so `apt` reported every package as 0.
#[test]
fn installed_size_is_reported_and_grows_with_the_payload() {
    let small = build(&sample_plan(), &Options::default());
    let small_size: u64 = small.control["Installed-Size"].parse().expect("a number");
    assert!(small_size > 0);

    let big = plan_with_files(
        vec![PlannedFile::inline(
            dest("/usr/lib/probe-app/big.bin"),
            vec![0u8; 512 * 1024],
            PlannedFile::MODE_REGULAR,
        )],
        "a probe",
    );
    let big_size: u64 = build(&big, &Options::default()).control["Installed-Size"]
        .parse()
        .expect("a number");
    assert!(
        big_size > small_size + 400,
        "half a megabyte should move the figure: {small_size} -> {big_size}"
    );
}

#[test]
fn maintainer_scripts_land_in_the_control_member() {
    let options = Options {
        maintainer_scripts: vec![
            ("postinst".to_owned(), b"#!/bin/sh\nexit 0\n".to_vec()),
            ("prerm".to_owned(), b"#!/bin/sh\nexit 0\n".to_vec()),
        ],
        ..Options::default()
    };
    assert_eq!(
        build(&sample_plan(), &options).scripts,
        vec!["postinst", "prerm"]
    );
}

#[test]
fn two_builds_of_one_plan_are_byte_identical() {
    let plan = sample_plan();
    let first = build_bytes(&plan, &Options::default()).expect("should build");
    let second = build_bytes(&plan, &Options::default()).expect("should build");
    assert_eq!(
        first, second,
        "the bash implementation failed this: two builds differed at byte 33"
    );
}

#[test]
fn a_different_timestamp_produces_different_bytes() {
    let plan = sample_plan();
    let mut other = sample_plan();
    other.timestamp = Timestamp::from_secs(TIMESTAMP + 1);

    let a = build_bytes(&plan, &Options::default()).expect("build");
    let b = build_bytes(&other, &Options::default()).expect("build");
    assert_ne!(a, b, "the timestamp must actually reach the archive");
}

#[test]
fn no_timestamp_anywhere_reflects_the_build_time() {
    let bytes = build_bytes(&sample_plan(), &Options::default()).expect("should build");
    let package = read::parse(&bytes).expect("should parse");
    for (path, entry) in &package.data {
        assert_eq!(entry.mtime, TIMESTAMP, "`{path}` carries a wall-clock time");
    }
    for (name, _, _, _, mtime) in ar_members(&bytes) {
        assert_eq!(
            mtime, TIMESTAMP,
            "ar member `{name}` carries a wall-clock time"
        );
    }
}

/// Two reference implementations disagreed on this; a real `dpkg-deb` output settled it.
#[test]
fn ar_member_headers_match_what_dpkg_deb_writes() {
    let bytes = build_bytes(&sample_plan(), &Options::default()).expect("should build");
    let members = ar_members(&bytes);
    assert_eq!(members.len(), 3);
    for (name, mode, uid, gid, _) in members {
        assert_eq!(
            mode, 0o100_644,
            "`{name}` mode must carry the file-type bits"
        );
        assert_eq!(uid, 0, "`{name}` uid");
        assert_eq!(gid, 0, "`{name}` gid");
    }
}

/// Dependency trees routinely exceed tar's 100-byte name field.
#[test]
fn a_path_longer_than_the_tar_name_field_round_trips() {
    let deep = format!(
        "/usr/lib/probe-app/app/node_modules/{}/index.js",
        ["nested-package-name"; 8].join("/node_modules/")
    );
    assert!(
        deep.len() > 100,
        "fixture must exceed the limit to test it: {}",
        deep.len()
    );

    let plan = plan_with_files(
        vec![PlannedFile::inline(
            dest(&deep),
            b"module.exports = 1\n".to_vec(),
            PlannedFile::MODE_REGULAR,
        )],
        "a probe",
    );
    let package = build(&plan, &Options::default());
    let expected = deep.trim_start_matches('/');
    assert!(
        package.data.contains_key(expected),
        "long path truncated or lost; got {:?}",
        package.data.keys().collect::<Vec<_>>()
    );
    assert!(package.md5sums.contains_key(expected));
}

#[test]
fn a_source_that_changed_since_planning_is_refused() {
    let dir = std::env::temp_dir().join("nativepkg-deb-source-changed");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let source = dir.join("app.js");
    std::fs::write(&source, b"original\n").expect("write");

    let plan = plan_with_files(
        vec![PlannedFile::from_source(
            dest("/usr/lib/probe-app/app.js"),
            source.clone(),
            b"original\n".len() as u64,
            false,
        )],
        "a probe",
    );
    // Grow the file after planning.
    std::fs::write(&source, b"original, but longer now\n").expect("rewrite");
    let result = build_bytes(&plan, &Options::default());
    std::fs::remove_file(&source).ok();

    let err = result.expect_err("a changed source must be detected");
    assert!(err.to_string().contains("app.js"), "{err}");
}

/// Checking `metadata()` alone leaves a window: if the length changes before the copy finishes,
/// every later tar entry is misparsed — or, when the delta lands inside one 512-byte block, the
/// file is padded while `md5sums` records what was read, and `dpkg --verify` reports
/// corruption. `/proc` gives the divergence deterministically: size zero, then content.
#[cfg(target_os = "linux")]
#[test]
fn a_stream_that_yields_a_different_length_than_declared_is_refused() {
    let procfile = PathBuf::from("/proc/self/status");
    let reported = std::fs::metadata(&procfile).expect("proc file").len();
    assert_eq!(
        reported, 0,
        "this test relies on /proc reporting a size of zero; it reported {reported}"
    );

    let plan = plan_with_files(
        vec![PlannedFile::from_source(
            dest("/usr/lib/probe-app/captured"),
            procfile,
            // Matches what the filesystem reports, so only a post-stream check can catch it.
            0,
            false,
        )],
        "a probe",
    );

    let err = build_bytes(&plan, &Options::default())
        .expect_err("a stream longer than its declared size must be refused");
    assert!(
        err.to_string().contains("bytes"),
        "the error should report the length mismatch: {err}"
    );
}

#[test]
fn file_contents_are_streamed_from_their_sources() {
    let dir = std::env::temp_dir().join("nativepkg-deb-streamed");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let source = dir.join("payload.bin");
    let content = vec![7u8; 4096];
    std::fs::write(&source, &content).expect("write");

    let plan = plan_with_files(
        vec![PlannedFile::from_source(
            dest("/usr/lib/probe-app/payload.bin"),
            source.clone(),
            content.len() as u64,
            false,
        )],
        "a probe",
    );
    let package = build(&plan, &Options::default());
    std::fs::remove_file(&source).ok();

    assert_eq!(
        package.data["usr/lib/probe-app/payload.bin"].size,
        content.len() as u64
    );
    let expected = md5_hex(&content);
    assert_eq!(package.md5sums["usr/lib/probe-app/payload.bin"], expected);
}

#[test]
fn dpkg_deb_accepts_the_package_when_available() {
    let Ok(probe) = Command::new("dpkg-deb").arg("--version").output() else {
        eprintln!("SKIP: `dpkg-deb` not on PATH; structural checks ran without it");
        return;
    };
    assert!(probe.status.success(), "dpkg-deb present but not runnable");

    let dir = std::env::temp_dir().join("nativepkg-deb-dpkg-check");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path: PathBuf = dir.join("probe.deb");
    std::fs::write(
        &path,
        build_bytes(&sample_plan(), &Options::default()).expect("build"),
    )
    .expect("write");

    let info = Command::new("dpkg-deb").arg("--info").arg(&path).output();
    let contents = Command::new("dpkg-deb")
        .arg("--contents")
        .arg(&path)
        .output();
    std::fs::remove_file(&path).ok();

    let info = info.expect("dpkg-deb was available a moment ago");
    let contents = contents.expect("dpkg-deb was available a moment ago");
    assert!(
        info.status.success(),
        "dpkg-deb --info rejected the package: {}",
        String::from_utf8_lossy(&info.stderr)
    );
    assert!(
        contents.status.success(),
        "dpkg-deb --contents rejected the package: {}",
        String::from_utf8_lossy(&contents.stderr)
    );
    let listing = String::from_utf8_lossy(&contents.stdout);
    assert!(listing.contains("app.js"), "{listing}");
}

/// Computed independently of the implementation, so digest assertions compare against a real
/// hash.
fn md5_hex(content: &[u8]) -> String {
    use core::fmt::Write as _;
    use md5::{Digest, Md5};
    Md5::digest(content)
        .iter()
        .fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// `(name, mode, uid, gid, mtime)` from every `ar` header.
fn ar_members(bytes: &[u8]) -> Vec<(String, u32, u32, u32, u64)> {
    let mut out = Vec::new();
    let mut offset = 8; // past "!<arch>\n"
    while offset + 60 <= bytes.len() {
        let header = &bytes[offset..offset + 60];
        let field = |range: std::ops::Range<usize>| {
            String::from_utf8_lossy(&header[range]).trim().to_owned()
        };
        let name = field(0..16);
        let mtime: u64 = field(16..28).parse().unwrap_or(u64::MAX);
        let uid: u32 = field(28..34).parse().unwrap_or(u32::MAX);
        let gid: u32 = field(34..40).parse().unwrap_or(u32::MAX);
        let mode = u32::from_str_radix(&field(40..48), 8).unwrap_or(0);
        let size: usize = field(48..58).parse().unwrap_or(0);
        out.push((name, mode, uid, gid, mtime));
        offset += 60 + size + (size % 2);
    }
    out
}

#[test]
fn triggers_are_a_control_member_only_when_supplied() {
    let plan = sample_plan();

    let without = build_bytes(&plan, &Options::default()).expect("builds");
    let package = read::parse(&without).expect("readable");
    assert!(
        !package.scripts.iter().any(|s| s == "triggers"),
        "no triggers were supplied, yet a triggers member exists: {:?}",
        package.scripts
    );

    let options = Options {
        triggers: Some(b"interest-noawait /usr/lib/probe-app\n".to_vec()),
        ..Options::default()
    };
    let with = build_bytes(&plan, &options).expect("builds");
    let package = read::parse(&with).expect("readable");
    assert_eq!(
        package.script_bodies.get("triggers").map(String::as_str),
        Some("interest-noawait /usr/lib/probe-app\n"),
        "the triggers member must carry the supplied bytes verbatim"
    );
}
