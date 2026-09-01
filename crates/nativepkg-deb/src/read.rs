//! Reading a `.deb` back, for tests: build, parse and assert on any host, with no `dpkg-deb`.
//! Host tooling is an extra layer when available, never a prerequisite.

use std::collections::BTreeMap;
use std::io::Read;

use crate::compression::Compression;
use crate::error::{Error, Result};

#[derive(Debug, Default)]
pub struct Package {
    pub members: Vec<String>,
    pub control: BTreeMap<String, String>,
    pub control_text: String,
    /// `data.tar` entries by path (without `./`). A map collapses duplicates; check
    /// [`Package::data_order`] for how many times a path actually appeared.
    pub data: BTreeMap<String, Entry>,
    /// Every `data.tar` entry path, in stream order, duplicates included.
    pub data_order: Vec<String>,
    pub md5sums: BTreeMap<String, String>,
    pub conffiles: Vec<String>,
    pub scripts: Vec<String>,
    /// A test reading the raw package bytes only sees what the container leaves uncompressed;
    /// that is how Debian tooling inside a compressed archive went unreported.
    pub script_bodies: BTreeMap<String, String>,
}

impl Package {
    /// Text of a regular entry, or empty if absent; for tests asserting on generated files.
    #[must_use]
    pub fn data_text(&self, path: &str) -> String {
        self.data
            .get(path)
            .and_then(|entry| entry.content.as_deref())
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn occurrences(&self, path: &str) -> usize {
        self.data_order.iter().filter(|p| *p == path).count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub kind: EntryKind,
    pub mode: u32,
    pub mtime: u64,
    pub uid: u64,
    pub gid: u64,
    pub size: u64,
    pub link_target: Option<String>,
    /// Bytes of a regular entry, so a test can assert on a generated file's text.
    pub content: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Regular,
    Directory,
    Symlink,
}

pub fn decompress(compression: Compression, bytes: &[u8]) -> Result<Vec<u8>> {
    match compression {
        Compression::None => Ok(bytes.to_vec()),
        Compression::Gzip => {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(bytes)
                .read_to_end(&mut out)
                .map_err(|e| Error::archive("gzip decompression failed", e))?;
            Ok(out)
        }
        Compression::Xz => {
            let mut out = Vec::new();
            liblzma::read::XzDecoder::new(bytes)
                .read_to_end(&mut out)
                .map_err(|e| Error::archive("xz decompression failed", e))?;
            Ok(out)
        }
        Compression::Zstd => {
            zstd::decode_all(bytes).map_err(|e| Error::archive("zstd decompression failed", e))
        }
    }
}

pub fn parse(bytes: &[u8]) -> Result<Package> {
    let mut package = Package::default();
    let mut archive = ar::Archive::new(std::io::Cursor::new(bytes));

    while let Some(entry) = archive.next_entry() {
        let mut entry = entry.map_err(|e| Error::archive("could not read an ar member", e))?;
        let name = String::from_utf8_lossy(entry.header().identifier()).into_owned();
        let mut contents = Vec::new();
        entry
            .read_to_end(&mut contents)
            .map_err(|e| Error::archive(format!("could not read member `{name}`"), e))?;
        package.members.push(name.clone());

        if let Some(stem) = name.strip_prefix("control.tar") {
            let image = decompress(compression_for(stem)?, &contents)?;
            read_control_tar(&image, &mut package)?;
        } else if let Some(stem) = name.strip_prefix("data.tar") {
            let image = decompress(compression_for(stem)?, &contents)?;
            read_data_tar(&image, &mut package)?;
        }
    }
    Ok(package)
}

fn compression_for(suffix: &str) -> Result<Compression> {
    match suffix {
        "" => Ok(Compression::None),
        ".gz" => Ok(Compression::Gzip),
        ".xz" => Ok(Compression::Xz),
        ".zst" => Ok(Compression::Zstd),
        other => Err(Error::Archive {
            reason: format!("unrecognised archive member suffix `{other}`"),
            source: None,
        }),
    }
}

fn read_control_tar(image: &[u8], package: &mut Package) -> Result<()> {
    let mut archive = tar::Archive::new(image);
    for entry in archive
        .entries()
        .map_err(|e| Error::archive("could not read the control tarball", e))?
    {
        let mut entry = entry.map_err(|e| Error::archive("could not read a control entry", e))?;
        let path = entry
            .path()
            .map_err(|e| Error::archive("a control entry has an unreadable path", e))?
            .to_string_lossy()
            .trim_start_matches("./")
            .to_owned();
        let mut text = String::new();
        entry
            .read_to_string(&mut text)
            .map_err(|e| Error::archive(format!("could not read control entry `{path}`"), e))?;

        match path.as_str() {
            "control" => {
                package.control = parse_control_fields(&text);
                package.control_text = text;
            }
            "md5sums" => {
                package.md5sums = text
                    .lines()
                    .filter_map(|line| line.split_once("  "))
                    .map(|(digest, file)| (file.to_owned(), digest.to_owned()))
                    .collect();
            }
            "conffiles" => {
                package.conffiles = text.lines().map(ToOwned::to_owned).collect();
            }
            other => {
                package.scripts.push(other.to_owned());
                package.script_bodies.insert(other.to_owned(), text);
            }
        }
    }
    package.scripts.sort_unstable();
    Ok(())
}

/// Joins folded continuation lines back onto their field.
fn parse_control_fields(text: &str) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    let mut current: Option<(String, String)> = None;

    for line in text.lines() {
        if line.starts_with(' ') {
            if let Some((_, value)) = current.as_mut() {
                value.push('\n');
                value.push_str(line.trim_start_matches(' '));
            }
        } else if let Some((key, value)) = line.split_once(": ") {
            if let Some((k, v)) = current.take() {
                fields.insert(k, v);
            }
            current = Some((key.to_owned(), value.to_owned()));
        }
    }
    if let Some((k, v)) = current {
        fields.insert(k, v);
    }
    fields
}

fn read_data_tar(image: &[u8], package: &mut Package) -> Result<()> {
    let mut archive = tar::Archive::new(image);
    for entry in archive
        .entries()
        .map_err(|e| Error::archive("could not read the data tarball", e))?
    {
        let mut entry = entry.map_err(|e| Error::archive("could not read a data entry", e))?;
        let header = entry.header();
        let path = entry
            .path()
            .map_err(|e| Error::archive("a data entry has an unreadable path", e))?
            .to_string_lossy()
            .trim_start_matches("./")
            .trim_end_matches('/')
            .to_owned();

        let kind = if header.entry_type().is_dir() {
            EntryKind::Directory
        } else if header.entry_type().is_symlink() {
            EntryKind::Symlink
        } else {
            EntryKind::Regular
        };

        let link_target = entry
            .link_name()
            .ok()
            .flatten()
            .map(|p| p.to_string_lossy().into_owned());

        // Header fields are copied out before the bytes are read: reading needs the entry
        // mutably and the header borrows it.
        let (mode, mtime, uid, gid, size) = (
            header.mode().unwrap_or(0),
            header.mtime().unwrap_or(0),
            header.uid().unwrap_or(u64::MAX),
            header.gid().unwrap_or(u64::MAX),
            header.size().unwrap_or(0),
        );
        let content = if kind == EntryKind::Regular {
            let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
            entry
                .read_to_end(&mut bytes)
                .map_err(|e| Error::archive(format!("could not read `{path}` from data.tar"), e))?;
            Some(bytes)
        } else {
            None
        };

        package.data_order.push(path.clone());
        package.data.insert(
            path,
            Entry {
                kind,
                mode,
                mtime,
                uid,
                gid,
                size,
                link_target,
                content,
            },
        );
    }
    Ok(())
}
