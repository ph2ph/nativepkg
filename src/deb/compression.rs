//! Compressors Debian accepts for the archive members, verified against `dpkg-deb` on the
//! pinned toolchain (`Allowed types: gzip, xz, zstd, none`; `-Z zstd` writes `data.tar.zst`).

use std::io::{self, Write};

use crate::deb::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Compression {
    Gzip,
    /// The default: what Debian's own archive uses and what policy accepts.
    #[default]
    Xz,
    /// Fastest by far (it parallelises) but not the default: policy names only gzip, xz, bzip2
    /// and lzma for archive members, and lintian reports anything else as
    /// `malformed-deb-archive`, an error. Needs dpkg 1.21.18 or newer.
    Zstd,
    None,
}

impl Compression {
    #[must_use]
    pub fn extension(self) -> &'static str {
        match self {
            Self::Gzip => "gz",
            Self::Xz => "xz",
            Self::Zstd => "zst",
            Self::None => "",
        }
    }

    /// The full member name for a tarball, e.g. `data.tar.zst`.
    #[must_use]
    pub fn member_name(self, stem: &str) -> String {
        match self {
            Self::None => format!("{stem}.tar"),
            _ => format!("{stem}.tar.{}", self.extension()),
        }
    }

    /// Parses a compressor name as `dpkg-deb` spells it.
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "gzip" | "gz" => Ok(Self::Gzip),
            "xz" => Ok(Self::Xz),
            "zstd" | "zst" => Ok(Self::Zstd),
            "none" => Ok(Self::None),
            other => Err(Error::Archive {
                reason: format!("unknown compressor `{other}`; accepted: gzip, xz, zstd, none"),
                source: None,
            }),
        }
    }

    /// Compresses a whole image in memory; [`Compression::encoder`] streams.
    pub fn compress(self, tar: &[u8]) -> Result<Vec<u8>> {
        let mut encoder = self.encoder(Vec::with_capacity(tar.len() / 2))?;
        encoder
            .write_all(tar)
            .map_err(|e| Error::archive("compression failed", e))?;
        encoder.finish()
    }

    /// A compressor writing straight into `sink`, for members too large to hold.
    pub fn encoder<W: Write>(self, sink: W) -> Result<Encoder<W>> {
        Ok(match self {
            Self::None => Encoder::None(sink),
            Self::Gzip => Encoder::Gzip(flate2::write::GzEncoder::new(
                sink,
                flate2::Compression::default(),
            )),
            Self::Xz => Encoder::Xz(liblzma::write::XzEncoder::new(sink, XZ_LEVEL)),
            Self::Zstd => Encoder::Zstd(
                zstd::Encoder::new(sink, ZSTD_LEVEL)
                    .map_err(|e| Error::archive("zstd compression failed", e))?,
            ),
        })
    }
}

/// One member's compressor. Same bytes as [`Compression::compress`], without the buffer.
pub enum Encoder<W: Write> {
    None(W),
    Gzip(flate2::write::GzEncoder<W>),
    Xz(liblzma::write::XzEncoder<W>),
    Zstd(zstd::Encoder<'static, W>),
}

impl<W: Write> Encoder<W> {
    /// Writes the trailer and hands the sink back.
    pub fn finish(self) -> Result<W> {
        match self {
            Self::None(sink) => Ok(sink),
            Self::Gzip(encoder) => encoder.finish(),
            Self::Xz(encoder) => encoder.finish(),
            Self::Zstd(encoder) => encoder.finish(),
        }
        .map_err(|e| Error::archive("compression failed", e))
    }
}

impl<W: Write> Write for Encoder<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::None(sink) => sink.write(buf),
            Self::Gzip(encoder) => encoder.write(buf),
            Self::Xz(encoder) => encoder.write(buf),
            Self::Zstd(encoder) => encoder.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::None(sink) => sink.flush(),
            Self::Gzip(encoder) => encoder.flush(),
            Self::Xz(encoder) => encoder.flush(),
            Self::Zstd(encoder) => encoder.flush(),
        }
    }
}

const XZ_LEVEL: u32 = 6;

/// Highest level within the ordinary window; what dpkg uses for its own high-compression builds.
const ZSTD_LEVEL: i32 = 19;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_names_match_what_dpkg_deb_writes() {
        assert_eq!(Compression::Gzip.member_name("data"), "data.tar.gz");
        assert_eq!(Compression::Xz.member_name("control"), "control.tar.xz");
        assert_eq!(Compression::Zstd.member_name("data"), "data.tar.zst");
        assert_eq!(Compression::None.member_name("data"), "data.tar");
    }

    #[test]
    fn every_compressor_round_trips_through_its_own_decoder() {
        let payload: Vec<u8> = (0..10_000u32).map(|n| (n % 251) as u8).collect();
        for compression in [
            Compression::Gzip,
            Compression::Xz,
            Compression::Zstd,
            Compression::None,
        ] {
            let compressed = compression
                .compress(&payload)
                .unwrap_or_else(|e| panic!("{compression:?} should compress: {e}"));
            let back = crate::deb::read::decompress(compression, &compressed)
                .unwrap_or_else(|e| panic!("{compression:?} should decompress: {e}"));
            assert_eq!(back, payload, "{compression:?} did not round-trip");
        }
    }

    #[test]
    fn compression_is_deterministic() {
        let payload = b"the same input twice".repeat(100);
        for compression in [Compression::Gzip, Compression::Xz, Compression::Zstd] {
            let a = compression.compress(&payload).expect("compress");
            let b = compression.compress(&payload).expect("compress");
            assert_eq!(a, b, "{compression:?} is not deterministic");
        }
    }

    #[test]
    fn compression_actually_compresses_repetitive_input() {
        let payload = b"aaaaaaaaaaaaaaaa".repeat(1000);
        for compression in [Compression::Gzip, Compression::Xz, Compression::Zstd] {
            let out = compression.compress(&payload).expect("compress");
            assert!(
                out.len() < payload.len() / 10,
                "{compression:?} produced {} bytes from {}",
                out.len(),
                payload.len()
            );
        }
    }

    #[test]
    fn names_are_parsed_as_dpkg_spells_them() {
        assert_eq!(Compression::parse("gzip").unwrap(), Compression::Gzip);
        assert_eq!(Compression::parse("zstd").unwrap(), Compression::Zstd);
        assert_eq!(Compression::parse("xz").unwrap(), Compression::Xz);
        assert_eq!(Compression::parse("none").unwrap(), Compression::None);
    }

    #[test]
    fn an_unknown_compressor_is_rejected_listing_the_accepted_names() {
        let err = Compression::parse("bzip2")
            .expect_err("unknown")
            .to_string();
        assert!(err.contains("bzip2"), "{err}");
        assert!(err.contains("zstd"), "{err}");
    }
}
