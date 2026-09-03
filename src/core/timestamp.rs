//! The single timestamp every archive entry carries, so identical inputs produce identical
//! archives. bash used `cp -pf` and let `dpkg-deb` stamp the wall clock: two builds of an
//! unchanged tree differed at byte 33.

use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::core::{Error, Result};

/// Environment variable defined by the reproducible-builds specification.
const SOURCE_DATE_EPOCH: &str = "SOURCE_DATE_EPOCH";

/// A build timestamp, as seconds since the Unix epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Timestamp(u64);

impl Timestamp {
    #[must_use]
    pub fn as_secs(self) -> u64 {
        self.0
    }

    #[must_use]
    pub fn from_secs(secs: u64) -> Self {
        Self(secs)
    }

    #[must_use]
    pub fn describe(self, source: TimestampSource) -> String {
        format!("{} ({})", self.0, source.describe())
    }
}

/// Which rung of the fallback chain supplied the timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TimestampSource {
    SourceDateEpoch,
    GitCommit,
    NewestSourceMtime,
}

impl TimestampSource {
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::SourceDateEpoch => "from SOURCE_DATE_EPOCH",
            Self::GitCommit => "from the current git commit",
            Self::NewestSourceMtime => "newest source mtime",
        }
    }
}

/// Resolves the build timestamp: `SOURCE_DATE_EPOCH`, else the git commit time (identical on
/// every machine building that commit), else the newest source mtime (stable for a rebuild of
/// an unchanged tree, not across fresh checkouts). Never the wall clock.
///
/// # Errors
///
/// [`Error::Manifest`] when `SOURCE_DATE_EPOCH` is set but malformed: whoever set it wants
/// reproducibility and must be told they are not getting it. [`Error::Io`] when the sources
/// cannot be inspected and no earlier rung applied.
pub fn resolve(project_root: &Path, sources: &[&Path]) -> Result<(Timestamp, TimestampSource)> {
    let from_env = std::env::var(SOURCE_DATE_EPOCH).ok();
    resolve_from(from_env.as_deref(), project_root, sources)
}

/// [`resolve`] with the environment passed in: mutating a process-global variable from tests
/// needs `unsafe` on edition 2024 and makes them race.
pub fn resolve_from(
    source_date_epoch: Option<&str>,
    project_root: &Path,
    sources: &[&Path],
) -> Result<(Timestamp, TimestampSource)> {
    if let Some(raw) = source_date_epoch {
        let trimmed = raw.trim();
        let secs = trimmed.parse::<u64>().map_err(|_| {
            Error::manifest(format!(
                "{SOURCE_DATE_EPOCH} is set to `{trimmed}`, which is not a non-negative \
                 integer; unset it or correct it, because a malformed value would silently \
                 cost you reproducible builds"
            ))
        })?;
        return Ok((Timestamp(secs), TimestampSource::SourceDateEpoch));
    }

    if let Some(secs) = git_commit_timestamp(project_root) {
        return Ok((Timestamp(secs), TimestampSource::GitCommit));
    }

    let newest = newest_mtime(sources)?;
    Ok((Timestamp(newest), TimestampSource::NewestSourceMtime))
}

/// `None` when this is not a usable checkout; that is ordinary, and a further rung follows.
fn git_commit_timestamp(project_root: &Path) -> Option<u64> {
    let output = Command::new("git")
        .args(["-C"])
        .arg(project_root)
        .args(["log", "-1", "--format=%ct"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

/// The newest modification time among the given paths, as seconds since the epoch.
fn newest_mtime(sources: &[&Path]) -> Result<u64> {
    let mut newest = 0_u64;
    for path in sources {
        let metadata = std::fs::metadata(path).map_err(|e| Error::io(*path, e))?;
        let modified = metadata.modified().map_err(|e| Error::io(*path, e))?;
        // A pre-epoch mtime is nonsense but not worth failing a build over; clamp it.
        let secs = secs_since_epoch(modified);
        newest = newest.max(secs);
    }
    if newest == 0 && sources.is_empty() {
        // Anything here would be arbitrary, so say so.
        return Err(Error::manifest(
            "cannot determine a build timestamp: no sources were given, \
             SOURCE_DATE_EPOCH is unset, and this is not a git checkout",
        ));
    }
    Ok(newest)
}

/// Converts a [`SystemTime`] to epoch seconds, clamping anything before the epoch to zero.
#[must_use]
pub fn secs_since_epoch(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
    }

    #[test]
    fn source_date_epoch_wins() {
        let (ts, source) =
            resolve_from(Some("1234567890"), &repo_root(), &[]).expect("should resolve");
        assert_eq!(ts.as_secs(), 1_234_567_890);
        assert_eq!(source, TimestampSource::SourceDateEpoch);
    }

    #[test]
    fn source_date_epoch_is_trimmed() {
        let (ts, _) = resolve_from(Some("  42\n"), &repo_root(), &[]).expect("should resolve");
        assert_eq!(ts.as_secs(), 42);
    }

    #[test]
    fn malformed_source_date_epoch_is_an_error() {
        let err = resolve_from(Some("not-a-number"), &repo_root(), &[])
            .expect_err("must not fall back silently");
        assert!(err.to_string().contains("SOURCE_DATE_EPOCH"), "{err}");
    }

    #[test]
    fn negative_source_date_epoch_is_an_error() {
        assert!(resolve_from(Some("-1"), &repo_root(), &[]).is_err());
    }

    #[test]
    fn empty_source_date_epoch_is_an_error() {
        assert!(resolve_from(Some(""), &repo_root(), &[]).is_err());
    }

    #[test]
    fn falls_back_to_the_commit_timestamp_in_a_checkout() {
        let (ts, source) =
            resolve_from(None, &repo_root(), &[]).expect("this repo is a git checkout");
        assert_eq!(source, TimestampSource::GitCommit);
        assert!(ts.as_secs() > 0);
    }

    #[test]
    fn resolution_is_stable_across_calls() {
        let a = resolve_from(None, &repo_root(), &[]).expect("should resolve");
        let b = resolve_from(None, &repo_root(), &[]).expect("should resolve");
        assert_eq!(a.0, b.0, "the timestamp must not be the wall clock");
    }

    #[test]
    fn newest_mtime_is_used_outside_a_checkout() {
        let dir = std::env::temp_dir().join("nativepkg-ts-newest-test");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("a.txt");
        std::fs::write(&file, "x").expect("write");
        let (ts, source) = resolve_from(None, &dir, &[file.as_path()]).expect("should resolve");
        std::fs::remove_file(&file).ok();
        assert_eq!(source, TimestampSource::NewestSourceMtime);
        assert!(ts.as_secs() > 0);
    }

    #[test]
    fn no_sources_and_no_checkout_is_an_error_rather_than_an_invented_value() {
        let dir = std::env::temp_dir().join("nativepkg-ts-empty-test");
        std::fs::create_dir_all(&dir).expect("temp dir");
        assert!(resolve_from(None, &dir, &[]).is_err());
    }

    #[test]
    fn source_is_describable() {
        assert!(TimestampSource::GitCommit.describe().contains("git"));
        assert!(
            Timestamp::from_secs(7)
                .describe(TimestampSource::SourceDateEpoch)
                .contains('7')
        );
    }
}
