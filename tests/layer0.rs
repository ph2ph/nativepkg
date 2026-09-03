//! Invariants that hold with no distribution tooling present: no `dpkg`, `rpm`, `pacman` or
//! container. The bash suite could assert almost nothing without them, which is why it was 49
//! sequential `docker run`s.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR")))
}

fn binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("test binary path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join("nativepkg")
}

fn build_all(tag: &str) -> (PathBuf, Vec<PathBuf>) {
    let source = repo_root().join("tests/fixtures/simple");
    let out = std::env::temp_dir().join(format!("nativepkg-layer0-{tag}"));
    let _ = std::fs::remove_dir_all(&out);

    let result = Command::new(binary())
        .current_dir(&source)
        .args(["--quiet", "--format", "deb,rpm,arch", "--maintainer"])
        .arg("A <a@example.com>")
        .arg("--output-dir")
        .arg(&out)
        .args(["package.json", "app.js", "lib"])
        .output()
        .expect("the binary should run");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let written: Vec<PathBuf> = std::fs::read_dir(&out)
        .expect("output directory")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    assert_eq!(written.len(), 3, "{written:?}");
    (out, written)
}

fn timestamps_in_archive(bytes: &[u8], name: &str) -> BTreeSet<u64> {
    if name.ends_with(".pkg.tar.zst") {
        let tar = zstd::decode_all(bytes).expect("a readable zstd stream");
        let mut archive = tar::Archive::new(&tar[..]);
        return archive
            .entries()
            .expect("entries")
            .filter_map(Result::ok)
            .filter_map(|entry| entry.header().mtime().ok())
            .collect();
    }

    // An `.rpm` keeps file mtimes in the header, so nothing needs decompressing.
    let package = rpm::Package::parse(&mut std::io::Cursor::new(bytes)).expect("a readable .rpm");
    package
        .metadata
        .get_file_entries()
        .expect("file entries")
        .iter()
        .map(|entry| u64::from(entry.modified_at().0))
        .collect()
}

/// A package that depends on the clock cannot be reproduced, and the failure is invisible: it
/// builds fine and differs from yesterday's. The timestamp comes from `SOURCE_DATE_EPOCH`, the
/// commit, or the newest source file — none of which may quietly be the clock.
#[test]
// Names are generated lowercase, and `.pkg.tar.zst` is not a single extension.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn no_output_carries_the_wall_clock() {
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after the epoch")
        .as_secs();

    let (out, written) = build_all("clock");

    let after = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after the epoch")
        .as_secs();

    // All three formats. An earlier version inspected the `.deb` alone — the format least
    // likely to break this, since the RPM writer stores `min(build stamp, source mtime)`.
    let mut inspected = 0;

    for path in &written {
        let bytes = std::fs::read(path).expect("the package");
        let name = path
            .file_name()
            .expect("a name")
            .to_string_lossy()
            .into_owned();

        let stamps: BTreeSet<u64> = if name.ends_with(".deb") {
            let package = nativepkg::deb::read::parse(&bytes).expect("a readable .deb");
            package.data.values().map(|entry| entry.mtime).collect()
        } else {
            timestamps_in_archive(&bytes, &name)
        };

        assert!(!stamps.is_empty(), "{name}: no entry was inspected");
        inspected += 1;

        for stamp in &stamps {
            assert!(
                *stamp < before || *stamp > after,
                "{name}: an entry is stamped {stamp}, which falls inside the window this build \
                 ran in ({before}..={after}) — that is the clock, not the plan"
            );
        }

        // A `.deb` carries one timestamp for the whole package. RPM stores `min(build stamp,
        // source mtime)` per file — still reproducible — so the stronger assertion is deb-only.
        if name.ends_with(".deb") {
            assert_eq!(
                stamps.len(),
                1,
                "{name}: expected a single timestamp: {stamps:?}"
            );
        }
    }

    assert_eq!(
        inspected, 3,
        "every format must be inspected, not just the readable one"
    );
    let _ = std::fs::remove_dir_all(&out);
}

/// The bash implementation needed `fakeroot`; ownership is written into the archive here, so
/// there is nothing to fake.
#[test]
fn every_format_builds_unprivileged_and_owns_its_files_as_root() {
    let (out, written) = build_all("unprivileged");

    let deb = written
        .iter()
        .find(|p| p.extension().is_some_and(|e| e == "deb"))
        .expect("a .deb");
    let bytes = std::fs::read(deb).expect("the package");
    let package = nativepkg::deb::read::parse(&bytes).expect("a readable .deb");

    for (path, entry) in &package.data {
        assert_eq!(entry.uid, 0, "`{path}` is not owned by root");
        assert_eq!(entry.gid, 0, "`{path}` is not group-owned by root");
    }

    let _ = std::fs::remove_dir_all(&out);
}
