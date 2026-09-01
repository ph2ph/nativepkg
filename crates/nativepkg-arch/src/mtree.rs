//! The `.MTREE` member: Arch's file manifest, a gzipped mtree listing with SHA-256 digests.
//!
//! A projection of the build plan, not a walk of a staged directory as `makepkg` does it: type,
//! mode, size and time come from plan entries and the digest from streaming the payload, which
//! is what keeps this backend free of a staging directory.

use core::fmt::Write as _;
use std::collections::BTreeMap;

use nativepkg_core::plan::{BuildPlan, EntryKind, PlannedFile};

const DEFAULT_MODE: u32 = 0o644;

/// One manifest line. `path` carries mtree's `./` prefix; `digest` is hex SHA-256.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub path: String,
    /// `file`, `dir` or `link`.
    pub kind: &'static str,
    pub mode: u32,
    pub size: u64,
    pub digest: Option<String>,
    pub link: Option<String>,
}

/// The record for one planned entry; `digest` is what streaming it produced.
#[must_use]
pub fn record_for(file: &PlannedFile, digest: Option<String>) -> Record {
    let path = format!("./{}", file.destination.relative_str());
    match &file.kind {
        EntryKind::Regular => Record {
            path,
            kind: "file",
            mode: file.mode,
            size: file.content.len(),
            digest,
            link: None,
        },
        EntryKind::Directory => Record {
            path,
            kind: "dir",
            mode: file.mode,
            size: 0,
            digest: None,
            link: None,
        },
        EntryKind::Symlink { target } => Record {
            path,
            kind: "link",
            mode: file.mode,
            size: 0,
            digest: None,
            link: Some(target.clone()),
        },
    }
}

/// Renders the manifest; it never lists `.MTREE` itself, matching `makepkg`.
#[must_use]
pub fn render(records: &[Record], plan: &BuildPlan) -> String {
    let time = plan.timestamp.as_secs();
    let mut out = String::with_capacity(records.len() * 96);

    out.push_str("#mtree\n");
    let _ = writeln!(out, "/set type=file uid=0 gid=0 mode={DEFAULT_MODE:o}");

    let mut current_mode = DEFAULT_MODE;
    for record in records {
        // `/set` defaults hold until the next `/set`; emitting one only when the mode changes is
        // what keeps the listing compact, as a real package's is.
        if record.kind == "file" && record.mode != current_mode {
            let _ = writeln!(out, "/set mode={:o}", record.mode);
            current_mode = record.mode;
        }

        let _ = write!(out, "{} time={time}.0", record.path);
        match record.kind {
            "dir" => {
                let _ = write!(out, " type=dir mode={:o}", record.mode);
            }
            "link" => {
                let _ = write!(out, " type=link mode={:o}", record.mode);
                if let Some(link) = &record.link {
                    let _ = write!(out, " link={link}");
                }
            }
            _ => {
                if record.mode != current_mode {
                    let _ = write!(out, " mode={:o}", record.mode);
                }
                let _ = write!(out, " size={}", record.size);
                if let Some(digest) = &record.digest {
                    let _ = write!(out, " sha256digest={digest}");
                }
            }
        }
        out.push('\n');
    }
    out
}

/// Parses a rendered manifest into `path -> attributes`, applying `/set` defaults. For tests, so
/// the manifest is checked without trusting the writer's notion of what it wrote.
#[must_use]
pub fn parse(text: &str) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut defaults: BTreeMap<String, String> = BTreeMap::new();
    let mut entries = BTreeMap::new();

    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("/set ") {
            for pair in rest.split_whitespace() {
                if let Some((key, value)) = pair.split_once('=') {
                    defaults.insert(key.to_owned(), value.to_owned());
                }
            }
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(path) = parts.next() else { continue };
        let mut attributes = defaults.clone();
        for pair in parts {
            if let Some((key, value)) = pair.split_once('=') {
                attributes.insert(key.to_owned(), value.to_owned());
            }
        }
        entries.insert(path.to_owned(), attributes);
    }
    entries
}
