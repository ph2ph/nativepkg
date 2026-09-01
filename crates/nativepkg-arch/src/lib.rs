//! Arch Linux (`.pkg.tar.zst`) backend for `nativepkg`.
//!
//! Consumes the format-agnostic build plan from [`nativepkg_core`] and writes a zstd tar holding
//! `.PKGINFO`, `.MTREE` and the payload, in-process: `makepkg` assumes an Arch host and is no
//! more installable on the CI runner than `rpmbuild`.
//!
//! `.BUILDINFO` is not emitted: it describes a `makepkg` build environment that does not exist
//! here, and `pacman` does not require it.

pub mod error;
pub mod install;
pub mod mtree;
pub mod pkginfo;

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use nativepkg_core::plan::{BuildPlan, EntryKind, FileContent, PlannedFile};
use nativepkg_core::scratch::{OutputFile, ScratchFile};
use sha2::{Digest, Sha256};
use tar::{Builder, EntryType, Header};

pub use error::{Error, Result};

const ROOT_NAME: &str = "root";

const MEMBER_PREFIX: &str = "./";

const INSTALL_MEMBER: &str = ".INSTALL";

/// Arch's own packages are built at 19.
const ZSTD_LEVEL: i32 = 19;
// io::copy reads through a BufReader's own buffer; without one it uses 8 KiB.
const READ_BUFFER: usize = 64 * 1024;

/// How to write a package.
///
/// Not `#[non_exhaustive]`: other crates build this with struct expressions, which that blocks.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// The `.INSTALL` script — one file of shell functions, not four, which is Arch's shape.
    /// Composed by the CLI, where the snippets and the format selection meet.
    pub install_scriptlet: Option<Vec<u8>>,
    /// Service whose unit `post_install` presets, so the package must ship the preset policy
    /// ([`nativepkg_core::build::systemd_preset_entry`]): Arch's `99-default.preset` says
    /// `disable *`, which leaves a unit running until reboot and disabled after it. Decided with
    /// [`install::uses_preset`].
    pub preset_service: Option<String>,
}

/// Writes a `.pkg.tar.zst` for `plan` into `output_dir`, returning the path written.
///
/// # Errors
///
/// [`Error::Io`] for the output or a source, [`Error::SourceChanged`] when a source's length no
/// longer matches the plan, [`Error::Archive`] when the archive cannot be assembled.
pub fn build(plan: &BuildPlan, options: &Options, output_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(output_dir).map_err(|e| Error::io(output_dir.to_path_buf(), e))?;
    let name = file_name(plan);
    let mut scratch = ScratchFile::in_dir(output_dir, &name)?;
    let output = OutputFile::create(output_dir.join(&name))?;
    let output = write_package(plan, options, output, &mut scratch)?;
    Ok(output.finish()?)
}

/// Builds the package in memory. For tests and small packages; [`build`] streams.
pub fn build_bytes(plan: &BuildPlan, options: &Options) -> Result<Vec<u8>> {
    let mut scratch = Cursor::new(Vec::new());
    write_package(plan, options, Vec::new(), &mut scratch)
}

/// `.MTREE` precedes the payload it describes, so the payload is written first into `scratch`,
/// hashed as it goes, and spliced in after the metadata: one read per source, memory bounded.
fn write_package<W: Write, S: Read + Write + Seek>(
    plan: &BuildPlan,
    options: &Options,
    sink: W,
    scratch: &mut S,
) -> Result<W> {
    let with_policy = with_preset(plan, options)?;
    let plan = &with_policy;
    let mtime = plan.timestamp.as_secs();

    let mut payload = Builder::new(&mut *scratch);
    payload.mode(tar::HeaderMode::Deterministic);
    let digests = write_payload(&mut payload, plan)?;
    // Entries only: the trailer is written once, by the outer builder.
    let payload_len = payload
        .get_mut()
        .stream_position()
        .map_err(|e| Error::archive("could not measure the payload", e))?;
    payload
        .finish()
        .map_err(|e| Error::archive("could not finish the payload", e))?;
    drop(payload);
    scratch
        .seek(SeekFrom::Start(0))
        .map_err(|e| Error::archive("could not rewind the payload", e))?;

    let generator = format!(
        "{} {}",
        plan.metadata.generator, plan.metadata.generator_version
    );
    let pkginfo = pkginfo::render(plan, &generator).into_bytes();
    let mut metadata: Vec<(&str, &[u8])> = vec![(".PKGINFO", &pkginfo)];
    if let Some(script) = &options.install_scriptlet {
        metadata.push((INSTALL_MEMBER, script));
    }
    let records = manifest_records(plan, &metadata, &digests)?;
    let manifest = mtree::render(&records, plan);

    let encoder = zstd::Encoder::new(sink, ZSTD_LEVEL)
        .map_err(|e| Error::archive("could not start zstd compression", e))?;
    let mut tar = Builder::new(encoder);
    tar.mode(tar::HeaderMode::Deterministic);
    append_bytes(
        &mut tar,
        ".MTREE",
        &gzip(manifest.as_bytes())?,
        0o644,
        mtime,
    )?;
    for (name, contents) in &metadata {
        append_bytes(&mut tar, name, contents, 0o644, mtime)?;
    }
    io::copy(&mut Read::take(&mut *scratch, payload_len), tar.get_mut())
        .map_err(|e| Error::archive("could not splice the payload", e))?;

    let encoder = tar
        .into_inner()
        .map_err(|e| Error::archive("could not finish the archive", e))?;
    encoder
        .finish()
        .map_err(|e| Error::archive("zstd compression failed", e))
}

/// The plan plus the preset policy for `options.preset_service`, or the plan as it was. Rebuilt
/// through [`BuildPlan::new`] so the preset directory is synthesised and lands in `.MTREE`.
fn with_preset(plan: &BuildPlan, options: &Options) -> Result<BuildPlan> {
    let Some(service) = &options.preset_service else {
        return Ok(plan.clone());
    };
    let mut files = plan.files.clone();
    files.push(nativepkg_core::build::systemd_preset_entry(service).map_err(Error::Core)?);
    BuildPlan::new(
        plan.identity.clone(),
        files,
        plan.timestamp,
        plan.metadata.clone(),
    )
    .map_err(Error::Core)
}

/// The conventional file name for a package.
#[must_use]
pub fn file_name(plan: &BuildPlan) -> String {
    let identity = &plan.identity;
    format!(
        "{}-{}-{}.pkg.tar.zst",
        identity.package_name,
        pkginfo::pkgver(&identity.version_rpm, &identity.release_rpm, identity.epoch),
        identity.architecture.arch_linux()
    )
}

/// The manifest records: metadata members, then every plan entry, regular files carrying the
/// digest computed while the payload was written.
fn manifest_records(
    plan: &BuildPlan,
    metadata: &[(&str, &[u8])],
    digests: &BTreeMap<String, String>,
) -> Result<Vec<mtree::Record>> {
    let mut records = Vec::with_capacity(plan.files.len() + metadata.len());

    // Metadata members are archive entries like any other; `.MTREE` alone is absent, since a
    // digest cannot cover the file carrying it.
    for (name, contents) in metadata {
        records.push(mtree::Record {
            path: format!("{MEMBER_PREFIX}{name}"),
            kind: "file",
            mode: 0o644,
            size: contents.len() as u64,
            digest: Some(hex(&Sha256::digest(contents))),
            link: None,
        });
    }

    // Ancestors are plan entries in their own right, so one walk records them all.
    for file in &plan.files {
        match &file.kind {
            EntryKind::Directory => records.push(mtree::Record {
                path: format!("{MEMBER_PREFIX}{}", file.destination.relative_str()),
                kind: "dir",
                mode: file.mode,
                size: 0,
                digest: None,
                link: None,
            }),
            EntryKind::Symlink { .. } => records.push(mtree::record_for(file, None)),
            EntryKind::Regular => {
                let digest = digests
                    .get(file.destination.relative_str())
                    .ok_or_else(|| Error::Archive {
                        reason: format!(
                            "`{}` was not written to the payload; this is a defect in the backend",
                            file.destination
                        ),
                        source: None,
                    })?;
                records.push(mtree::record_for(file, Some(digest.clone())));
            }
        }
    }
    Ok(records)
}

/// Writes the payload entries, returning each regular file's digest by relative path.
fn write_payload<W: Write>(
    tar: &mut Builder<W>,
    plan: &BuildPlan,
) -> Result<BTreeMap<String, String>> {
    let mtime = plan.timestamp.as_secs();
    let mut digests = BTreeMap::new();

    for file in &plan.files {
        match &file.kind {
            EntryKind::Directory => {
                let mut header = new_header(EntryType::Directory, file.mode, mtime);
                header.set_size(0);
                tar.append_data(
                    &mut header,
                    format!("{MEMBER_PREFIX}{}/", file.destination.relative_str()),
                    io::empty(),
                )
                .map_err(|e| {
                    Error::archive(format!("could not add directory `{}`", file.destination), e)
                })?;
            }
            EntryKind::Symlink { target } => {
                let mut header = new_header(EntryType::Symlink, PlannedFile::MODE_SYMLINK, mtime);
                header.set_size(0);
                header.set_link_name(target).map_err(|e| {
                    Error::archive(format!("could not record symlink target `{target}`"), e)
                })?;
                tar.append_data(
                    &mut header,
                    format!("{MEMBER_PREFIX}{}", file.destination.relative_str()),
                    io::empty(),
                )
                .map_err(|e| {
                    Error::archive(format!("could not add symlink `{}`", file.destination), e)
                })?;
            }
            EntryKind::Regular => {
                let digest = append_regular(tar, file, mtime)?;
                digests.insert(file.destination.relative_str().to_owned(), digest);
            }
        }
    }
    Ok(digests)
}

/// Reader that counts and hashes the bytes passing through it.
struct HashingReader<R> {
    inner: R,
    hasher: Sha256,
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

/// Appends a regular file, streaming and hashing it from its source.
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
            Ok(hex(&Sha256::digest(bytes)))
        }
        FileContent::FromPath { path, len } => {
            let handle = File::open(path).map_err(|e| Error::io(path.clone(), e))?;
            header.set_size(*len);
            let mut reader = HashingReader {
                inner: io::BufReader::with_capacity(READ_BUFFER, handle),
                hasher: Sha256::new(),
                bytes: 0,
            };
            builder
                .append_data(&mut header, member, &mut reader)
                .map_err(|e| Error::archive(format!("could not add `{}`", file.destination), e))?;

            // The header carries the planned length and `tar` pads by what it actually read, so
            // a source that changed size would misalign every later entry.
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

fn new_header(kind: EntryType, mode: u32, mtime: u64) -> Header {
    let mut header = Header::new_gnu();
    header.set_entry_type(kind);
    header.set_mode(mode);
    header.set_mtime(mtime);
    header.set_uid(0);
    header.set_gid(0);
    let _ = header.set_username(ROOT_NAME);
    let _ = header.set_groupname(ROOT_NAME);
    header
}

fn append_bytes<W: Write>(
    builder: &mut Builder<W>,
    name: &str,
    contents: &[u8],
    mode: u32,
    mtime: u64,
) -> Result<()> {
    let mut header = new_header(EntryType::Regular, mode, mtime);
    header.set_size(contents.len() as u64);
    builder
        .append_data(&mut header, format!("{MEMBER_PREFIX}{name}"), contents)
        .map_err(|e| Error::archive(format!("could not add `{name}`"), e))
}

/// `pacman` expects the manifest gzipped.
fn gzip(input: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(input)
        .map_err(|e| Error::archive("could not compress the manifest", e))?;
    encoder
        .finish()
        .map_err(|e| Error::archive("could not compress the manifest", e))
}

fn hex(bytes: &[u8]) -> String {
    use core::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nativepkg_core::plan::Destination;

    /// Single pass: the digest is of the bytes written; a size change is still refused.
    #[test]
    fn the_digest_is_of_the_bytes_written() {
        let dir = std::env::temp_dir().join("nativepkg-arch-single-pass");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("payload.js");
        std::fs::write(&path, b"AAAA").expect("write source");

        let file = PlannedFile {
            destination: Destination::new("/usr/lib/app/payload.js").expect("valid destination"),
            kind: EntryKind::Regular,
            mode: 0o644,
            content: FileContent::FromPath {
                path: path.clone(),
                len: 4,
            },
            is_config: false,
        };

        let mut builder = Builder::new(Vec::new());
        let digest = append_regular(&mut builder, &file, 0).expect("streams");
        assert_eq!(digest, hex(&Sha256::digest(b"AAAA")));

        std::fs::write(&path, b"BBBB").expect("same-length edit");
        let mut builder = Builder::new(Vec::new());
        let digest = append_regular(&mut builder, &file, 0).expect("streams");
        assert_eq!(digest, hex(&Sha256::digest(b"BBBB")));

        std::fs::write(&path, b"BBBBB").expect("size change");
        let mut builder = Builder::new(Vec::new());
        let error = append_regular(&mut builder, &file, 0).expect_err("size changed");
        assert!(matches!(error, Error::SourceChanged { .. }), "{error:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
