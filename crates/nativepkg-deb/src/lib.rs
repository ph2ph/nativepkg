//! Debian (`.deb`) backend for `nativepkg`: an `ar` container holding `debian-binary`,
//! `control.tar.*` and `data.tar.*`, assembled in-process from the core's build plan. Neither
//! `dpkg-deb` nor `fakeroot` is needed on the build host — ownership is stated in the plan,
//! which is all `fakeroot` ever provided.
//!
//! Must not depend on `nativepkg_rpm` or the CLI, nor read the manifest; everything arrives
//! through the plan.

pub mod archive;
pub mod compression;
pub mod control;
pub mod docs;
pub mod error;
pub mod read;
pub mod scripts;

#[cfg(test)]
mod test_support;

use core::fmt::Write as _;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use nativepkg_core::plan::BuildPlan;
use nativepkg_core::scratch::{OutputFile, ScratchFile};

pub use compression::Compression;
pub use error::{Error, Result};

/// The container format version.
const DEBIAN_BINARY: &[u8] = b"2.0\n";

/// `dpkg-deb` writes `100644` on `ar` members, file-type bits included; two reference
/// implementations disagreed, and a package `dpkg-deb` itself built settled it.
const AR_MEMBER_MODE: u32 = 0o100_644;

/// How to write a package. Not `#[non_exhaustive]`: this is built by `nativepkg-cli` and by
/// tests in other crates, and that attribute forbids `..Default::default()` there (E0639).
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub compression: Compression,
    /// Defaults to `misc`.
    pub section: Option<String>,
    /// `(name, contents)` pairs placed in `control.tar`. An input rather than part of the plan,
    /// so the format-agnostic core need not know `control.tar`'s layout.
    pub maintainer_scripts: Vec<(String, Vec<u8>)>,

    /// Contents of a `triggers` control file, passed through verbatim (as cargo-deb does): the
    /// directives are dpkg's grammar, and reinterpreting them could only go wrong.
    pub triggers: Option<Vec<u8>>,
}

/// Writes a `.deb` for `plan` into `output_dir`, returning the path written.
///
/// # Errors
///
/// [`Error::Io`] when the output cannot be written or a source read, [`Error::SourceChanged`]
/// when a file's length no longer matches the plan, [`Error::Archive`] when assembly fails.
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

/// The data member goes through `scratch` first: `md5sums`, which lives in the control member
/// and precedes the data member in the `ar` container, needs every digest before the container
/// can start. Memory stays at the control tarball plus buffers regardless of payload size.
fn write_package<W: Write, S: Read + Write + Seek>(
    plan: &BuildPlan,
    options: &Options,
    sink: W,
    scratch: &mut S,
) -> Result<W> {
    let plan = with_documentation(plan)?;
    let compression = options.compression;
    let mtime = plan.timestamp.as_secs();

    let encoder = compression.encoder(&mut *scratch)?;
    let (encoder, digests) = archive::write_data_archive(&plan, encoder)?;
    encoder
        .finish()?
        .flush()
        .map_err(|e| Error::archive("could not flush the data member", e))?;
    let data_len = scratch
        .stream_position()
        .map_err(|e| Error::archive("could not measure the data member", e))?;
    scratch
        .seek(SeekFrom::Start(0))
        .map_err(|e| Error::archive("could not rewind the data member", e))?;

    let control_text = control::render(&plan, options.section.as_deref());
    let md5sums = render_md5sums(&digests);
    let conffiles = render_conffiles(&plan);

    let mut members: Vec<(&str, Vec<u8>, u32)> = vec![
        ("control", control_text.into_bytes(), 0o644),
        ("md5sums", md5sums.into_bytes(), 0o644),
    ];
    if let Some(conffiles) = conffiles {
        members.push(("conffiles", conffiles.into_bytes(), 0o644));
    }
    for (name, contents) in &options.maintainer_scripts {
        members.push((name.as_str(), contents.clone(), 0o755));
    }
    if let Some(triggers) = &options.triggers {
        members.push(("triggers", triggers.clone(), 0o644));
    }
    let control_tar = archive::control_archive(&members, mtime)?;
    let control_member = compression.compress(&control_tar)?;

    let mut container = ar::Builder::new(sink);
    append_member(
        &mut container,
        "debian-binary",
        DEBIAN_BINARY.len() as u64,
        DEBIAN_BINARY,
        mtime,
    )?;
    append_member(
        &mut container,
        &compression.member_name("control"),
        control_member.len() as u64,
        &control_member[..],
        mtime,
    )?;
    append_member(
        &mut container,
        &compression.member_name("data"),
        data_len,
        Read::take(&mut *scratch, data_len),
        mtime,
    )?;

    container
        .into_inner()
        .map_err(|e| Error::archive("could not finish the ar container", e))
}

#[must_use]
pub fn file_name(plan: &BuildPlan) -> String {
    format!(
        "{}_{}_{}.deb",
        plan.identity.package_name,
        plan.identity.version_deb,
        plan.identity.architecture.deb()
    )
}

fn with_documentation(plan: &BuildPlan) -> Result<BuildPlan> {
    let mut files = plan.files.clone();
    files.extend(docs::entries(plan)?);
    BuildPlan::new(
        plan.identity.clone(),
        files,
        plan.timestamp,
        plan.metadata.clone(),
    )
    .map_err(Error::Core)
}

/// One `<digest>  <path>` line per regular file, path without its leading slash.
fn render_md5sums(digests: &std::collections::BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(digests.len() * 48);
    for (path, digest) in digests {
        let _ = writeln!(out, "{digest}  {path}");
    }
    out
}

fn render_conffiles(plan: &BuildPlan) -> Option<String> {
    let config = plan.config_files();
    if config.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(config.len() * 32);
    for file in config {
        let _ = writeln!(out, "{}", file.destination);
    }
    Some(out)
}

/// Appends one `ar` member with the metadata `dpkg-deb` writes.
fn append_member<W: Write, R: Read>(
    builder: &mut ar::Builder<W>,
    name: &str,
    size: u64,
    contents: R,
    mtime: u64,
) -> Result<()> {
    let mut header = ar::Header::new(name.as_bytes().to_vec(), size);
    header.set_mode(AR_MEMBER_MODE);
    header.set_mtime(mtime);
    header.set_uid(0);
    header.set_gid(0);
    builder
        .append(&header, contents)
        .map_err(|e| Error::archive(format!("could not add ar member `{name}`"), e))
}
