//! Building the two tarballs a `.deb` contains.
//!
//! Lintian wants an entry for every parent directory, with a trailing separator. `node_modules`
//! paths exceed tar's 100-byte name field, so GNU long names are mandatory. Headers are
//! deterministic with an explicit mtime, so identical input gives identical bytes. Contents are
//! streamed and hashed in one pass, serving both `md5sums` and the payload.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Write};

use crate::core::plan::{BuildPlan, EntryKind, FileContent, PlannedFile};
use md5::{Digest, Md5};
use tar::{Builder, EntryType, Header};

use crate::deb::error::{Error, Result};

/// Stated rather than inherited from disk; that is what removes the `fakeroot` dependency.
const ROOT_NAME: &str = "root";

/// `dpkg-deb` writes every member path with this prefix.
const MEMBER_PREFIX: &str = "./";
// io::copy reads through a BufReader's own buffer; without one it uses 8 KiB.
const READ_BUFFER: usize = 64 * 1024;

/// Lowercase hex. `md-5` 0.11 dropped `LowerHex` on its output type, and a hex crate for
/// sixteen bytes is not worth it.
fn hex(bytes: &[u8]) -> String {
    use core::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Reader that MD5s everything read through it, so a file is archived and hashed in one pass.
struct HashingReader<R> {
    inner: R,
    hasher: Md5,
    bytes: u64,
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.hasher.update(&buf[..read]);
        self.bytes += read as u64;
        Ok(read)
    }
}

/// Writes the data tarball into `sink` and returns each regular file's MD5, keyed by
/// destination without its leading slash — what `md5sums` wants.
pub fn write_data_archive<W: Write>(
    plan: &BuildPlan,
    sink: W,
) -> Result<(W, BTreeMap<String, String>)> {
    let mut builder = Builder::new(sink);
    builder.mode(tar::HeaderMode::Deterministic);
    let mtime = plan.timestamp.as_secs();
    let mut digests = BTreeMap::new();

    // Ancestors were synthesised when the plan was built, parents before children, so there is
    // no separate directory pass here to forget (one backend did).
    for file in &plan.files {
        match &file.kind {
            EntryKind::Directory => {
                append_directory(
                    &mut builder,
                    file.destination.relative_str(),
                    file.mode,
                    mtime,
                )?;
            }
            EntryKind::Symlink { target } => append_symlink(&mut builder, file, target, mtime)?,
            EntryKind::Regular => {
                let digest = append_regular(&mut builder, file, mtime)?;
                digests.insert(file.destination.relative_str().to_owned(), digest);
            }
        }
    }

    let sink = builder
        .into_inner()
        .map_err(|e| Error::archive("could not finish the data tarball", e))?;
    Ok((sink, digests))
}

/// Builds `control.tar` from `(name, contents, mode)` members, in the order given.
pub fn control_archive(members: &[(&str, Vec<u8>, u32)], mtime: u64) -> Result<Vec<u8>> {
    let mut builder = Builder::new(Vec::with_capacity(8192));
    builder.mode(tar::HeaderMode::Deterministic);

    for (name, contents, mode) in members {
        let mut header = new_header(EntryType::Regular, *mode, mtime);
        header.set_size(contents.len() as u64);
        builder
            .append_data(&mut header, format!("{MEMBER_PREFIX}{name}"), &contents[..])
            .map_err(|e| {
                Error::archive(format!("could not add `{name}` to the control tarball"), e)
            })?;
    }

    builder
        .into_inner()
        .map_err(|e| Error::archive("could not finish the control tarball", e))
}

/// A header with the fields every entry shares.
fn new_header(kind: EntryType, mode: u32, mtime: u64) -> Header {
    let mut header = Header::new_gnu();
    header.set_entry_type(kind);
    header.set_mode(mode);
    header.set_mtime(mtime);
    header.set_uid(0);
    header.set_gid(0);
    // Names as well as ids, matching `dpkg-deb --root-owner-group`.
    let _ = header.set_username(ROOT_NAME);
    let _ = header.set_groupname(ROOT_NAME);
    header
}

/// Appends a directory entry, with the trailing separator lintian requires.
fn append_directory<W: Write>(
    builder: &mut Builder<W>,
    destination: &str,
    mode: u32,
    mtime: u64,
) -> Result<()> {
    let mut header = new_header(EntryType::Directory, mode, mtime);
    header.set_size(0);
    builder
        .append_data(
            &mut header,
            format!("{MEMBER_PREFIX}{destination}/"),
            io::empty(),
        )
        .map_err(|e| Error::archive(format!("could not add directory `{destination}`"), e))
}

fn append_symlink<W: Write>(
    builder: &mut Builder<W>,
    file: &PlannedFile,
    target: &str,
    mtime: u64,
) -> Result<()> {
    let mut header = new_header(EntryType::Symlink, PlannedFile::MODE_SYMLINK, mtime);
    header.set_size(0);
    header
        .set_link_name(target)
        .map_err(|e| Error::archive(format!("could not record symlink target `{target}`"), e))?;
    builder
        .append_data(
            &mut header,
            format!("{MEMBER_PREFIX}{}", file.destination.relative_str()),
            io::empty(),
        )
        .map_err(|e| Error::archive(format!("could not add symlink `{}`", file.destination), e))
}

/// Appends a regular file, streaming and hashing it, and returns its hex digest.
fn append_regular<W: Write>(
    builder: &mut Builder<W>,
    file: &PlannedFile,
    mtime: u64,
) -> Result<String> {
    let mut header = new_header(EntryType::Regular, file.mode, mtime);
    let member = format!("{MEMBER_PREFIX}{}", file.destination.relative_str());

    match &file.content {
        FileContent::Inline(bytes) => {
            header.set_size(bytes.len() as u64);
            builder
                .append_data(&mut header, member, &bytes[..])
                .map_err(|e| Error::archive(format!("could not add `{}`", file.destination), e))?;
            Ok(hex(&Md5::digest(bytes)))
        }
        FileContent::FromPath { path, len } => {
            let handle = File::open(path).map_err(|e| Error::io(path.clone(), e))?;
            let actual = handle
                .metadata()
                .map_err(|e| Error::io(path.clone(), e))?
                .len();
            if actual != *len {
                // A source edited between planning and writing must fail, not ship half-read.
                return Err(Error::SourceChanged {
                    path: path.clone(),
                    planned: *len,
                    actual,
                });
            }

            header.set_size(*len);
            let mut reader = HashingReader {
                inner: io::BufReader::with_capacity(READ_BUFFER, handle),
                hasher: Md5::new(),
                bytes: 0,
            };
            builder
                .append_data(&mut header, member, &mut reader)
                .map_err(|e| Error::archive(format!("could not add `{}`", file.destination), e))?;

            // The header carries the planned length. If the file changed size after the check
            // above, the data region no longer matches and every later entry is misparsed — or,
            // within one 512-byte block, `dpkg --verify` reports corruption on the target.
            if reader.bytes != *len {
                return Err(Error::SourceChanged {
                    path: path.clone(),
                    planned: *len,
                    actual: reader.bytes,
                });
            }
            Ok(hex(&reader.hasher.finalize()))
        }
        FileContent::None => Err(Error::Archive {
            reason: format!(
                "`{}` is a regular file with no content; this is a defect in the plan",
                file.destination
            ),
            source: None,
        }),
    }
}
