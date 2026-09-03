//! Files a backend writes through on its way to a package.
//!
//! A payload can be larger than memory, so a backend streams it into a scratch file beside the
//! output and copies it into the container afterwards. Both guards here remove their file on
//! drop unless told otherwise, so a failed build leaves nothing behind.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::core::error::{Error, Result};

/// A read-write temporary file, removed on drop.
#[derive(Debug)]
pub struct ScratchFile {
    path: PathBuf,
    file: File,
}

impl ScratchFile {
    /// Creates `<dir>/.<stem>.tmp`, replacing anything already there.
    pub fn in_dir(dir: &Path, stem: &str) -> Result<Self> {
        let path = dir.join(format!(".{stem}.tmp"));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options.open(&path).map_err(|e| Error::io(&path, e))?;
        Ok(Self { path, file })
    }
}

impl Read for ScratchFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }
}

impl Write for ScratchFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Seek for ScratchFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.file.seek(pos)
    }
}

impl Drop for ScratchFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// The package being written. Removed on drop unless [`OutputFile::finish`] was called, so an
/// error part-way never leaves a truncated package that looks complete.
#[derive(Debug)]
pub struct OutputFile {
    path: PathBuf,
    writer: BufWriter<File>,
    keep: bool,
}

impl OutputFile {
    /// Creates (or truncates) `path`.
    pub fn create(path: PathBuf) -> Result<Self> {
        let file = File::create(&path).map_err(|e| Error::io(&path, e))?;
        Ok(Self {
            path,
            writer: BufWriter::new(file),
            keep: false,
        })
    }

    /// Flushes and keeps the file, returning its path. A failed flush leaves it to be removed.
    pub fn finish(mut self) -> Result<PathBuf> {
        self.writer.flush().map_err(|e| Error::io(&self.path, e))?;
        self.keep = true;
        Ok(self.path.clone())
    }
}

impl Write for OutputFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writer.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

impl Drop for OutputFile {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_file(&self.path);
        }
    }
}
