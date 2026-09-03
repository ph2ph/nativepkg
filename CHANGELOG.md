# Changelog

## Unreleased

A Rust rewrite that emits `.deb`, `.rpm` and `.pkg.tar.zst` from one build plan,
configured by a `.nativepkg` file or command-line flags, with no host packaging
toolchain (`dpkg-deb`, `rpmbuild`, `makepkg` or `fakeroot`) required.

### Added
- Published on crates.io as a single crate: `cargo install nativepkg`.
- `.rpm` and `.pkg.tar.zst` output alongside `.deb`; `--format` selects any
  combination.
- Byte-reproducible builds, from `SOURCE_DATE_EPOCH`, the git commit time or the
  newest source file.
- `--dry-run` with machine-readable output, describing exactly what would be
  built.
- `copyright`, `changelog.Debian.gz` and `Installed-Size` in generated packages,
  as Debian policy requires.
- Selectable compression (`gzip`, `xz`, `zstd`), defaulting to zstd.
- Install-at-unpack runs `npm install --omit=dev` by default, or any command set
  with `--install-command`; there is no package-manager detection.
- Configuration from a `.nativepkg` file, or entirely from command-line flags;
  `package.json` is never read.
- `--nodejs` adds a `nodejs` dependency; `--deps` declares any others.
- Warnings for config keys that are not settings, naming the nearest real key.

### Fixed
- `--install-dir` did nothing: its assignment was misspelled, so the value was
  parsed and discarded.
- A manifest with no `author` produced `Maintainer: null`, a malformed package.
- A multi-line description aborted template rendering.
- Scoped npm names corrupted paths, and `1.2.3-beta.1` sorted above `1.2.3`.
- The systemd unit was installed to `/lib/systemd/system` and used `Requires=`
  with no ordering.
- Maintainer scripts ignored dpkg's action argument, so `prerm` wiped
  `node_modules` during an upgrade.
- `npm install --unsafe-perm` ran as root, over the network, at installation
  time.
- The staging directory was created inside the project tree and could end up
  inside the package.

### Changed
- The payload installs under `/usr/lib` rather than `/usr/share`, which Debian
  policy reserves for architecture-independent data.
- Pre-release versions are mapped: `1.2.3-beta.1` becomes `1.2.3~beta.1`, so it
  sorts below its release.
- Package names are normalised: `@scope/name` becomes `scope-name`.
- A package with no maintainer is refused rather than shipped malformed.
- Neither `sudo` nor `nodejs` is a default dependency; `nodejs` is added by
  `--nodejs`.
