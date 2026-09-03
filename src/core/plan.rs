//! The build plan: the sole contract between this crate and the format backends.
//!
//! A plan describes what a package contains without saying how any format encodes it.
//! Backends consume it and nothing else: they cannot read `package.json` or reach into the
//! source tree except through the paths a plan entry names.
//!
//! Content is lazy: a regular file records its source path and length, not its bytes, so a
//! backend streams each source straight into its archive. Everything reproducibility needs
//! (one timestamp, sorted entries, explicit ownership and mode) is in the plan, so no backend
//! implements it.

use core::fmt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::arch::Architecture;
use crate::core::timestamp::Timestamp;
use crate::core::{Error, Result};

/// Installed-size allowance per file, following `cargo-deb`: round up to a whole kibibyte
/// (`KIB - 1` is the bias) and charge one more (`OVERHEAD_KIB`). `dpkg` counts per-file
/// overhead, and a plain byte sum badly understates a tree of thousands of small files. The
/// identity `(len + this) / KIB == ceil(len / KIB) + OVERHEAD_KIB` is asserted in the tests.
const BYTES_PER_FILE_OVERHEAD: u64 = (KIB - 1) + (OVERHEAD_KIB * KIB);

const OVERHEAD_KIB: u64 = 1;

const KIB: u64 = 1024;

/// An absolute, normalised destination path inside the package. Only [`Destination::new`]
/// builds one: the bash implementation concatenated strings, which is how a scoped npm name
/// (`@acme/app`) corrupted a path and aborted a build mid-way.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Destination(String);

impl Destination {
    /// Normalises an absolute path lexically: resolves `.` and `..` without touching the
    /// filesystem, collapses repeated separators, and refuses anything that ascends above the
    /// root.
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return Err(Error::manifest("destination path is empty"));
        }
        if !path.is_absolute() {
            return Err(Error::manifest(format!(
                "destination `{}` must be absolute",
                path.display()
            )));
        }

        let mut parts: Vec<String> = Vec::new();
        for component in path.components() {
            match component {
                Component::RootDir | Component::Prefix(_) | Component::CurDir => {}
                Component::ParentDir => {
                    if parts.pop().is_none() {
                        return Err(Error::manifest(format!(
                            "destination `{}` escapes the package root",
                            path.display()
                        )));
                    }
                }
                Component::Normal(part) => {
                    let part = part.to_string_lossy();
                    // Unix allows control characters in filenames, but `md5sums` and
                    // `conffiles` are line-oriented: a newline splits one entry into two.
                    // Rejecting here catches it for every backend at once.
                    if let Some(bad) = part.chars().find(|c| c.is_control()) {
                        return Err(Error::manifest(format!(
                            "destination `{}` contains the control character U+{:04X}; package \
                             metadata is line-oriented and cannot represent it",
                            path.display(),
                            bad as u32
                        )));
                    }
                    if !part.is_empty() {
                        parts.push(part.into_owned());
                    }
                }
            }
        }

        if parts.is_empty() {
            return Err(Error::manifest(format!(
                "destination `{}` resolves to the package root, which cannot be an entry",
                path.display()
            )));
        }

        Ok(Self(format!("/{}", parts.join("/"))))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Without the leading separator: the form tar members and `md5sums` entries use.
    #[must_use]
    pub fn relative_str(&self) -> &str {
        self.0.trim_start_matches('/')
    }

    /// The parent destinations, outermost first. Debian tooling requires a directory entry
    /// for every directory containing a packaged file.
    #[must_use]
    pub fn ancestors(&self) -> Vec<Self> {
        let parts: Vec<&str> = self.relative_str().split('/').collect();
        (1..parts.len())
            .map(|n| Self(format!("/{}", parts[..n].join("/"))))
            .collect()
    }

    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        let relative = self.relative_str();
        let (head, _) = relative.rsplit_once('/')?;
        Some(Self(format!("/{head}")))
    }

    /// The first segment, which decides whether a symlink may be relative.
    fn top_level(&self) -> &str {
        self.relative_str().split('/').next().unwrap_or_default()
    }

    /// The target a symlink at `self` should carry to point at `target`. Per Debian policy and
    /// `dh_link`: relative when both ends share a top-level directory, absolute otherwise. The
    /// bash implementation always wrote absolute, which lintian flags for the `/usr/bin` →
    /// `/usr/share` link it generates on every build.
    #[must_use]
    pub fn link_target_from(&self, target: &Self) -> String {
        if self.top_level() != target.top_level() {
            return target.0.clone();
        }

        let from: Vec<&str> = self.relative_str().split('/').collect();
        let to: Vec<&str> = target.relative_str().split('/').collect();
        // The link's own final component is the link name, not a directory to ascend from.
        let from_dir = &from[..from.len().saturating_sub(1)];

        let shared = from_dir
            .iter()
            .zip(to.iter())
            .take_while(|(a, b)| a == b)
            .count();

        let ups = from_dir.len() - shared;
        let mut parts: Vec<&str> = std::iter::repeat_n("..", ups).collect();
        parts.extend_from_slice(&to[shared..]);
        if parts.is_empty() {
            // The target is the link's own directory. An empty string is not a valid symlink
            // target; `.` is the self-referential form.
            return ".".to_owned();
        }
        parts.join("/")
    }
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Regular,
    Directory,
    Symlink {
        /// As it should be stored, computed by [`Destination::link_target_from`].
        target: String,
    },
}

/// Where an entry's bytes come from: application files reference their source so a backend
/// can stream them; generated files (scripts, units, the control file) carry their bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileContent {
    FromPath {
        path: PathBuf,
        /// Recorded at plan time, so a backend can detect a source that changed underneath it.
        len: u64,
    },
    Inline(Vec<u8>),
    /// Directories and symlinks.
    None,
}

impl FileContent {
    #[must_use]
    pub fn len(&self) -> u64 {
        match self {
            Self::FromPath { len, .. } => *len,
            Self::Inline(bytes) => bytes.len() as u64,
            Self::None => 0,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One thing the package installs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedFile {
    pub destination: Destination,
    pub kind: EntryKind,
    pub content: FileContent,
    /// Permission bits, without file-type bits.
    pub mode: u32,
    /// Whether the target's package manager should preserve local edits to this file.
    pub is_config: bool,
}

impl PlannedFile {
    pub const MODE_EXECUTABLE: u32 = 0o755;
    pub const MODE_REGULAR: u32 = 0o644;
    pub const MODE_DIRECTORY: u32 = 0o755;
    pub const MODE_SYMLINK: u32 = 0o777;

    #[must_use]
    pub fn from_source(
        destination: Destination,
        path: PathBuf,
        len: u64,
        executable: bool,
    ) -> Self {
        Self {
            destination,
            kind: EntryKind::Regular,
            content: FileContent::FromPath { path, len },
            mode: if executable {
                Self::MODE_EXECUTABLE
            } else {
                Self::MODE_REGULAR
            },
            is_config: false,
        }
    }

    #[must_use]
    pub fn inline(destination: Destination, bytes: Vec<u8>, mode: u32) -> Self {
        Self {
            destination,
            kind: EntryKind::Regular,
            content: FileContent::Inline(bytes),
            mode,
            is_config: false,
        }
    }

    #[must_use]
    pub fn directory(destination: Destination) -> Self {
        Self {
            destination,
            kind: EntryKind::Directory,
            content: FileContent::None,
            mode: Self::MODE_DIRECTORY,
            is_config: false,
        }
    }

    /// A symlink, with its target normalised per Debian policy.
    #[must_use]
    pub fn symlink(destination: Destination, target: &Destination) -> Self {
        let stored = destination.link_target_from(target);
        Self {
            destination,
            kind: EntryKind::Symlink { target: stored },
            content: FileContent::None,
            mode: Self::MODE_SYMLINK,
            is_config: false,
        }
    }

    /// A symlink whose stored target is supplied verbatim. Prefer [`PlannedFile::symlink`],
    /// which applies the relativity policy.
    #[must_use]
    pub fn symlink_to(destination: Destination, target: impl Into<String>) -> Self {
        Self {
            destination,
            kind: EntryKind::Symlink {
                target: target.into(),
            },
            content: FileContent::None,
            mode: Self::MODE_SYMLINK,
            is_config: false,
        }
    }

    #[must_use]
    pub fn is_symlink(&self) -> bool {
        matches!(self.kind, EntryKind::Symlink { .. })
    }

    #[must_use]
    pub fn as_config(mut self) -> Self {
        self.is_config = true;
        self
    }

    #[must_use]
    pub fn is_regular(&self) -> bool {
        self.kind == EntryKind::Regular
    }
}

/// A package description, split the way both target formats want it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Description {
    /// First line: Debian's `Description:` value and RPM's `Summary`.
    pub synopsis: String,
    /// Remaining lines, empty when there are none.
    pub body: String,
}

impl Description {
    /// Refuses empty or whitespace-only text: both formats require a synopsis.
    pub fn split(raw: &str) -> Result<Self> {
        let mut lines = raw.lines();
        let synopsis = lines
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Error::manifest("description is empty; both package formats require a synopsis")
            })?;

        let body = lines
            .collect::<Vec<_>>()
            .join("\n")
            .trim_matches('\n')
            .to_owned();

        Ok(Self {
            synopsis: synopsis.to_owned(),
            body,
        })
    }
}

/// Who the package is, independent of how it is encoded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub package_name: String,
    /// Epoch included.
    pub version_deb: String,
    pub version_rpm: String,
    pub release_rpm: String,
    /// Carried separately for formats with their own epoch field.
    pub epoch: Option<u32>,
    pub description: Description,
    pub maintainer: String,
    pub architecture: Architecture,
    pub dependencies: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
}

/// Provenance of the plan itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanMetadata {
    pub generator: String,
    pub generator_version: String,
}

/// Everything a backend needs to emit a package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildPlan {
    pub identity: Identity,
    /// Sorted by destination, no duplicates, every ancestor directory present.
    pub files: Vec<PlannedFile>,
    /// One timestamp for every entry.
    pub timestamp: Timestamp,
    pub metadata: PlanMetadata,
}

impl BuildPlan {
    /// Every entry is written as root. Stating ownership in the plan is what removes the
    /// `fakeroot` dependency.
    pub const UID: u32 = 0;
    pub const GID: u32 = 0;

    /// Sorts entries, rejects duplicate destinations, and adds every ancestor directory.
    pub fn new(
        identity: Identity,
        mut files: Vec<PlannedFile>,
        timestamp: Timestamp,
        metadata: PlanMetadata,
    ) -> Result<Self> {
        files.sort_by(|a, b| a.destination.cmp(&b.destination));

        // Sorted, so duplicates are adjacent: one scan and no allocation over a list of tens
        // of thousands of entries.
        if let Some(pair) = files
            .windows(2)
            .find(|w| w[0].destination == w[1].destination)
        {
            return Err(Error::manifest(format!(
                "two entries claim the destination `{}`; refusing to guess which should win",
                pair[0].destination
            )));
        }

        // Ancestors are the plan's job, not each backend's: three backends each computed them
        // in a separate pass, the RPM one lacked it, and `rpm --erase` left empty directories
        // behind until a container run found them.
        Self::add_ancestor_directories(&mut files)?;

        Ok(Self {
            identity,
            files,
            timestamp,
            metadata,
        })
    }

    /// Inserts a `0755` directory entry for every missing ancestor (an explicit directory
    /// keeps its own mode) and re-sorts so parents precede children.
    ///
    /// Fails when an ancestor is already occupied by something that is not a directory. The
    /// first version only looked at existing `Directory` entries, so a file at `/var/log/app`
    /// beside `/var/log/app/x.log` produced two entries at one path after the duplicate check
    /// had run; every archive writer accepted it and `tar` silently replaced the user's file
    /// with a directory. Reachable through `extra-files`.
    fn add_ancestor_directories(files: &mut Vec<PlannedFile>) -> Result<()> {
        use std::collections::{BTreeMap, BTreeSet};

        // What already occupies each path, not just which directories exist.
        let occupied: BTreeMap<&str, EntryKind> = files
            .iter()
            .map(|f| (f.destination.as_str(), f.kind.clone()))
            .collect();

        let mut missing: BTreeSet<Destination> = BTreeSet::new();
        for file in files.iter() {
            for ancestor in file.destination.ancestors() {
                let path = ancestor.as_str();
                if path == "/" {
                    continue;
                }
                match occupied.get(path) {
                    Some(EntryKind::Directory) => {}
                    Some(_) => {
                        return Err(Error::manifest(format!(
                            "`{}` must be a directory, because `{}` lives inside it, but the \
                             plan places a file there; a path cannot be both",
                            path, file.destination
                        )));
                    }
                    None => {
                        missing.insert(ancestor);
                    }
                }
            }
        }

        for destination in missing {
            files.push(PlannedFile::directory(destination));
        }
        files.sort_by(|a, b| a.destination.cmp(&b.destination));
        Ok(())
    }

    /// Installed size in kibibytes with per-file overhead; symlinks and directories count
    /// nothing.
    #[must_use]
    pub fn installed_size_kib(&self) -> u64 {
        self.files
            .iter()
            .filter(|f| f.is_regular())
            .map(|f| (f.content.len() + BYTES_PER_FILE_OVERHEAD) / KIB)
            .sum()
    }

    #[must_use]
    pub fn config_files(&self) -> Vec<&PlannedFile> {
        self.files.iter().filter(|f| f.is_config).collect()
    }

    /// For inspection; the shape is a debugging aid, not a stable interface.
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| Error::manifest(format!("could not render the plan as JSON: {e}")))
    }
}

#[cfg(test)]
mod tests {
    /// Review got `Ok` here, with a file and a synthesised directory both at `/var/log/app`;
    /// `tar` then silently replaced the file with a directory.
    #[test]
    fn a_file_where_a_directory_is_needed_is_refused() {
        let err = plan(vec![
            PlannedFile::from_source(dest("/var/log/app"), "a".into(), 1, false),
            PlannedFile::from_source(dest("/var/log/app/x.log"), "x".into(), 1, false),
        ])
        .expect_err("a path cannot be both a file and a directory");
        let msg = err.to_string();
        assert!(msg.contains("/var/log/app"), "{msg}");
        assert!(msg.contains("/var/log/app/x.log"), "{msg}");
        assert!(msg.contains("directory"), "{msg}");
    }

    #[test]
    fn a_symlink_where_a_directory_is_needed_is_refused() {
        let err = plan(vec![
            PlannedFile::symlink_to(dest("/usr/lib/app/bin"), "../elsewhere"),
            PlannedFile::from_source(dest("/usr/lib/app/bin/tool"), "t".into(), 1, false),
        ])
        .expect_err("a symlink cannot be an ancestor");
        assert!(err.to_string().contains("/usr/lib/app/bin"), "{err}");
    }

    #[test]
    fn synthesis_never_produces_a_duplicate_destination() {
        let p = plan(vec![
            PlannedFile::directory(dest("/opt/app")),
            PlannedFile::from_source(dest("/opt/app/a"), "a".into(), 1, false),
            PlannedFile::from_source(dest("/opt/app/lib/b"), "b".into(), 1, false),
        ])
        .unwrap();
        let mut seen = std::collections::BTreeSet::new();
        for f in &p.files {
            assert!(
                seen.insert(f.destination.as_str()),
                "`{}` appears twice",
                f.destination
            );
        }
    }

    /// Three backends each had a separate ancestor pass and one lacked it.
    #[test]
    fn every_ancestor_directory_is_an_entry_of_the_plan() {
        let p = plan(vec![PlannedFile::from_source(
            dest("/usr/lib/app/lib/deep/file.js"),
            "f".into(),
            1,
            false,
        )])
        .unwrap();

        let directories: Vec<&str> = p
            .files
            .iter()
            .filter(|f| f.kind == EntryKind::Directory)
            .map(|f| f.destination.as_str())
            .collect();
        assert_eq!(
            directories,
            [
                "/usr",
                "/usr/lib",
                "/usr/lib/app",
                "/usr/lib/app/lib",
                "/usr/lib/app/lib/deep"
            ]
        );
        assert!(
            !directories.contains(&"/"),
            "the filesystem root is never an entry"
        );
    }

    #[test]
    fn an_explicit_directory_keeps_its_mode_when_ancestors_are_added() {
        let mut explicit = PlannedFile::directory(dest("/var/log/app"));
        explicit.mode = 0o750;
        let p = plan(vec![
            explicit,
            PlannedFile::from_source(dest("/var/log/app/x.log"), "x".into(), 1, false),
        ])
        .unwrap();

        let mode_of = |path: &str| {
            p.files
                .iter()
                .find(|f| f.destination.as_str() == path)
                .map(|f| f.mode)
        };
        assert_eq!(mode_of("/var/log/app"), Some(0o750));
        assert_eq!(mode_of("/var/log"), Some(PlannedFile::MODE_DIRECTORY));
        assert_eq!(
            p.files
                .iter()
                .filter(|f| f.destination.as_str() == "/var/log/app")
                .count(),
            1,
            "an explicit directory must not be duplicated by a synthesised one"
        );
    }

    #[test]
    fn parents_precede_children() {
        let p = plan(vec![PlannedFile::from_source(
            dest("/opt/x/y/z"),
            "z".into(),
            1,
            false,
        )])
        .unwrap();
        let order: Vec<&str> = p.files.iter().map(|f| f.destination.as_str()).collect();
        assert_eq!(order, ["/opt", "/opt/x", "/opt/x/y", "/opt/x/y/z"]);
    }

    use super::*;

    fn dest(p: &str) -> Destination {
        Destination::new(p).expect("test path should normalise")
    }

    #[test]
    fn destinations_normalise_dot_segments() {
        assert_eq!(dest("/usr/./share/../lib/app").as_str(), "/usr/lib/app");
    }

    #[test]
    fn destinations_collapse_repeated_separators() {
        assert_eq!(dest("/usr//lib///app").as_str(), "/usr/lib/app");
    }

    #[test]
    fn destinations_reject_escape() {
        let err = Destination::new("/usr/../../etc/passwd").unwrap_err();
        assert!(err.to_string().contains("escapes"), "{err}");
    }

    #[test]
    fn destinations_reject_relative_paths() {
        assert!(Destination::new("usr/lib/app").is_err());
    }

    #[test]
    fn destinations_reject_the_root_itself() {
        assert!(Destination::new("/").is_err());
        assert!(Destination::new("/..").is_err());
    }

    #[test]
    fn destinations_reject_control_characters() {
        // A newline here would split one `md5sums` line into two bogus ones.
        for bad in [
            "/usr/lib/app/two\nlines",
            "/usr/lib/app/tab\there",
            "/usr/lib/app/nul\0",
        ] {
            let err = Destination::new(bad).unwrap_err().to_string();
            // The previous message had thirty literal spaces in it from a line wrap without
            // a continuation, which a keyword `contains` could not see.
            assert!(
                !err.contains("  "),
                "the message should not contain runs of spaces: {err}"
            );
            assert!(
                err.contains("package metadata is line-oriented"),
                "the message should explain why: {err}"
            );
            assert!(
                err.contains("control character"),
                "`{}` should be refused: {err}",
                bad.escape_debug()
            );
        }
    }

    #[test]
    fn destinations_accept_ordinary_awkward_characters() {
        // The fixture suite has a `whitespace` project for this.
        for good in ["/usr/lib/app/with space", "/usr/lib/app/with'quote"] {
            assert!(
                Destination::new(good).is_ok(),
                "`{good}` should be accepted"
            );
        }
    }

    #[test]
    fn destinations_reject_empty() {
        assert!(Destination::new("").is_err());
    }

    #[test]
    fn relative_form_drops_the_leading_separator() {
        assert_eq!(dest("/usr/lib/app").relative_str(), "usr/lib/app");
    }

    #[test]
    fn ancestors_are_outermost_first() {
        let a = dest("/usr/lib/app/bin/x");
        let names: Vec<String> = a.ancestors().iter().map(ToString::to_string).collect();
        assert_eq!(
            names,
            ["/usr", "/usr/lib", "/usr/lib/app", "/usr/lib/app/bin"]
        );
    }

    #[test]
    fn symlink_within_one_top_level_directory_is_relative() {
        // The case the bash implementation gets wrong on every build.
        let link = dest("/usr/bin/app");
        let target = dest("/usr/lib/app/bin/app");
        assert_eq!(link.link_target_from(&target), "../lib/app/bin/app");
    }

    #[test]
    fn symlink_across_top_level_directories_is_absolute() {
        let link = dest("/etc/app/current");
        let target = dest("/usr/lib/app");
        assert_eq!(link.link_target_from(&target), "/usr/lib/app");
    }

    #[test]
    fn symlink_to_its_own_directory_is_self_referential_not_empty() {
        // An empty target is not a valid symlink.
        let link = dest("/usr/lib/app/current");
        let target = dest("/usr/lib/app");
        assert_eq!(link.link_target_from(&target), ".");
    }

    #[test]
    fn every_computed_link_target_is_non_empty() {
        let cases = [
            ("/usr/bin/app", "/usr/lib/app/bin/app"),
            ("/etc/app/current", "/usr/lib/app"),
            ("/usr/lib/app/current", "/usr/lib/app/v1"),
            ("/usr/lib/app/current", "/usr/lib/app"),
            ("/usr/a", "/usr/b"),
        ];
        for (link, target) in cases {
            let computed = dest(link).link_target_from(&dest(target));
            assert!(
                !computed.is_empty(),
                "`{link}` -> `{target}` produced an empty target"
            );
        }
    }

    #[test]
    fn overhead_constant_matches_its_stated_identity() {
        for len in [0_u64, 1, 1023, 1024, 1025, 4096, 100_000] {
            let via_constant = (len + BYTES_PER_FILE_OVERHEAD) / KIB;
            let via_intent = len.div_ceil(KIB) + OVERHEAD_KIB;
            assert_eq!(via_constant, via_intent, "identity broken for len = {len}");
        }
    }

    #[test]
    fn symlink_within_the_same_directory_needs_no_ascent() {
        let link = dest("/usr/lib/app/current");
        let target = dest("/usr/lib/app/v1");
        assert_eq!(link.link_target_from(&target), "v1");
    }

    #[test]
    fn description_splits_synopsis_from_body() {
        let d = Description::split("a tool\n\ndoes things").unwrap();
        assert_eq!(d.synopsis, "a tool");
        assert_eq!(d.body, "does things");
    }

    #[test]
    fn single_line_description_has_no_body() {
        let d = Description::split("a tool").unwrap();
        assert_eq!(d.synopsis, "a tool");
        assert!(d.body.is_empty());
    }

    #[test]
    fn empty_description_is_rejected() {
        assert!(Description::split("").is_err());
        assert!(Description::split("   \n  ").is_err());
    }

    fn identity() -> Identity {
        Identity {
            package_name: "app".into(),
            version_deb: "1.0.0".into(),
            version_rpm: "1.0.0".into(),
            release_rpm: "1".into(),
            epoch: None,
            description: Description::split("a tool").unwrap(),
            maintainer: "A <a@example.com>".into(),
            architecture: Architecture::Any,
            dependencies: Some("nodejs".into()),
            homepage: None,
            license: None,
        }
    }

    fn metadata() -> PlanMetadata {
        PlanMetadata {
            generator: "nativepkg".into(),
            generator_version: "0.1.0".into(),
        }
    }

    fn plan(files: Vec<PlannedFile>) -> Result<BuildPlan> {
        BuildPlan::new(identity(), files, Timestamp::from_secs(1000), metadata())
    }

    #[test]
    fn entries_are_sorted_by_destination() {
        let p = plan(vec![
            PlannedFile::from_source(dest("/usr/b"), "b".into(), 1, false),
            PlannedFile::from_source(dest("/usr/a"), "a".into(), 1, false),
        ])
        .unwrap();
        let order: Vec<&str> = p.files.iter().map(|f| f.destination.as_str()).collect();
        // `/usr` is synthesised at construction.
        assert_eq!(order, ["/usr", "/usr/a", "/usr/b"]);
    }

    #[test]
    fn duplicate_destinations_are_rejected() {
        let err = plan(vec![
            PlannedFile::from_source(dest("/usr/a"), "one".into(), 1, false),
            PlannedFile::from_source(dest("/usr/a"), "two".into(), 1, false),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("/usr/a"), "{err}");
    }

    #[test]
    fn installed_size_allows_per_file_overhead() {
        // Round up to a whole kibibyte, then add one: a one-byte file costs 2 KiB, not 0.
        let p = plan(vec![
            PlannedFile::from_source(dest("/usr/a"), "a".into(), 1, false),
            PlannedFile::from_source(dest("/usr/b"), "b".into(), 1, false),
            PlannedFile::from_source(dest("/usr/c"), "c".into(), 1, false),
        ])
        .unwrap();
        assert_eq!(p.installed_size_kib(), 6);
    }

    #[test]
    fn installed_size_scales_with_content() {
        let p = plan(vec![PlannedFile::from_source(
            dest("/usr/big"),
            "big".into(),
            10 * 1024,
            false,
        )])
        .unwrap();
        // 10 KiB of content plus one KiB of overhead.
        assert_eq!(p.installed_size_kib(), 11);
    }

    #[test]
    fn symlinks_and_directories_do_not_inflate_installed_size() {
        let target = dest("/usr/lib/app/bin/app");
        let p = plan(vec![
            PlannedFile::directory(dest("/usr/lib/app")),
            PlannedFile::symlink(dest("/usr/bin/app"), &target),
        ])
        .unwrap();
        assert_eq!(p.installed_size_kib(), 0);
    }

    #[test]
    fn modes_follow_entry_kind() {
        let target = dest("/usr/lib/app/bin/app");
        assert_eq!(
            PlannedFile::directory(dest("/usr/lib/app")).mode,
            PlannedFile::MODE_DIRECTORY
        );
        assert_eq!(
            PlannedFile::symlink(dest("/usr/bin/app"), &target).mode,
            PlannedFile::MODE_SYMLINK
        );
        assert_eq!(
            PlannedFile::from_source(dest("/usr/a"), "a".into(), 1, true).mode,
            PlannedFile::MODE_EXECUTABLE
        );
        assert_eq!(
            PlannedFile::from_source(dest("/usr/a"), "a".into(), 1, false).mode,
            PlannedFile::MODE_REGULAR
        );
    }

    #[test]
    fn config_files_are_listed_in_order() {
        let p = plan(vec![
            PlannedFile::from_source(dest("/etc/app/b.conf"), "b".into(), 1, false).as_config(),
            PlannedFile::from_source(dest("/usr/a"), "a".into(), 1, false),
            PlannedFile::from_source(dest("/etc/app/a.conf"), "a".into(), 1, false).as_config(),
        ])
        .unwrap();
        let names: Vec<&str> = p
            .config_files()
            .iter()
            .map(|f| f.destination.as_str())
            .collect();
        assert_eq!(names, ["/etc/app/a.conf", "/etc/app/b.conf"]);
    }

    #[test]
    fn plan_round_trips_through_json() {
        let p = plan(vec![
            PlannedFile::from_source(dest("/usr/a"), "a".into(), 3, false),
            PlannedFile::inline(dest("/usr/b"), b"xyz".to_vec(), 0o644),
        ])
        .unwrap();
        let json = p.to_json().unwrap();
        let back: BuildPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn json_lists_every_destination() {
        let p = plan(vec![PlannedFile::from_source(
            dest("/usr/lib/app/app.js"),
            "app.js".into(),
            10,
            false,
        )])
        .unwrap();
        assert!(p.to_json().unwrap().contains("/usr/lib/app/app.js"));
    }

    #[test]
    fn long_destinations_are_recorded_in_full() {
        // node_modules paths routinely exceed tar's 100-byte header limit.
        let deep = format!(
            "/usr/lib/app/node_modules/{}/index.js",
            "a/node_modules/b".repeat(8)
        );
        let d = Destination::new(&deep).unwrap();
        assert!(d.as_str().len() > 100);
        assert!(d.as_str().ends_with("/index.js"));
    }
}
