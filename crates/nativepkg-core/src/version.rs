//! Mapping semantic versions onto native package versions that sort correctly.
//!
//! npm separates a pre-release with `-`, but in a Debian version `-` starts the revision, which
//! sorts *above* no revision: `1.2.3-beta.1` verbatim tells `apt` the beta is newer than
//! `1.2.3`, and a machine on the pre-release never upgrades. Both Debian and RPM sort `~` below
//! the empty string, so one mapping serves both:
//!
//! | npm | Debian / RPM `Version` |
//! |---|---|
//! | `1.2.3` | `1.2.3` |
//! | `1.2.3-beta.1` | `1.2.3~beta.1` |
//! | `1.2.3-rc.1+build.5` | `1.2.3~rc.1+build.5` |

use core::fmt;

use crate::{Error, Result};

/// A version before mapping. An explicit override may be a version the distribution already
/// dictates rather than semver; modelling both keeps that escape hatch without silently
/// mapping or refusing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionSpec {
    Semver(semver::Version),
    /// Not semver but already valid for the target formats; passed through untouched.
    Literal(String),
}

impl VersionSpec {
    /// Parses a version, preferring semver and falling back to a literal.
    pub fn parse(input: &str) -> Result<Self> {
        if let Ok(v) = semver::Version::parse(input) {
            return Ok(Self::Semver(v));
        }
        validate_upstream(input)?;
        Ok(Self::Literal(input.to_owned()))
    }

    /// Whether this version passes through without mapping, so the caller can warn.
    #[must_use]
    pub fn is_literal(&self) -> bool {
        matches!(self, Self::Literal(_))
    }
}

impl fmt::Display for VersionSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Semver(v) => write!(f, "{v}"),
            Self::Literal(s) => f.write_str(s),
        }
    }
}

/// A version rendered for every target format. Debian carries the epoch inside the version
/// (`1:1.2.3`), RPM in a separate tag: [`MappedVersion::deb`] is epoch-prefixed and
/// [`MappedVersion::rpm_version`] is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappedVersion {
    epoch: Option<u32>,
    deb: String,
    rpm_version: String,
    rpm_release: String,
    hyphens_rewritten: bool,
}

impl MappedVersion {
    /// Default RPM release for a package built from source we control.
    const DEFAULT_RPM_RELEASE: &'static str = "1";

    /// Maps a [`VersionSpec`] onto every target format.
    pub fn new(spec: &VersionSpec, epoch: Option<u32>) -> Result<Self> {
        let (base, hyphens_rewritten) = match spec {
            VersionSpec::Semver(v) => map_semver(v),
            VersionSpec::Literal(s) => (s.clone(), false),
        };

        // Mapping is total, so this can only fire for a `Literal`. Report against what the
        // user wrote, or the message is untraceable back to `package.json`.
        validate_upstream(&base).map_err(|e| match e {
            Error::InvalidVersion { reason, .. } => Error::InvalidVersion {
                version: spec.to_string(),
                reason: format!("{reason} (after mapping to `{base}`)"),
            },
            other => other,
        })?;

        let deb = match epoch {
            Some(e) => format!("{e}:{base}"),
            None => base.clone(),
        };

        Ok(Self {
            epoch,
            deb,
            rpm_version: base,
            rpm_release: Self::DEFAULT_RPM_RELEASE.to_owned(),
            hyphens_rewritten,
        })
    }

    #[must_use]
    pub fn deb(&self) -> &str {
        &self.deb
    }

    #[must_use]
    pub fn rpm_version(&self) -> &str {
        &self.rpm_version
    }

    #[must_use]
    pub fn rpm_release(&self) -> &str {
        &self.rpm_release
    }

    #[must_use]
    pub fn epoch(&self) -> Option<u32> {
        self.epoch
    }

    /// Whether a `-` inside a semver identifier was rewritten to `.`; callers warn, since
    /// `alpha-1` and `alpha.1` collide.
    #[must_use]
    pub fn hyphens_rewritten(&self) -> bool {
        self.hyphens_rewritten
    }
}

/// Renders a semver with `~` for pre-release and `+` for build metadata. Semver permits `-`
/// inside an identifier (`21AF26D3---117B344092BD` is semver.org's own example) but neither
/// format can carry it in a revision-less version, so it becomes `.` — lossy, hence the flag.
fn map_semver(v: &semver::Version) -> (String, bool) {
    let mut out = format!("{}.{}.{}", v.major, v.minor, v.patch);
    let mut rewritten = false;

    let mut push_identifiers = |out: &mut String, text: &str| {
        for ch in text.chars() {
            if ch == '-' {
                out.push('.');
                rewritten = true;
            } else {
                out.push(ch);
            }
        }
    };

    if !v.pre.is_empty() {
        out.push('~');
        let pre = v.pre.as_str().to_owned();
        push_identifiers(&mut out, &pre);
    }
    if !v.build.is_empty() {
        out.push('+');
        let build = v.build.as_str().to_owned();
        push_identifiers(&mut out, &build);
    }
    (out, rewritten)
}

/// Debian policy 5.6.12's upstream-version grammar, minus `-` (native packages have no
/// revision, and a stray `-` would create one) and `:` (the epoch is applied separately).
fn validate_upstream(candidate: &str) -> Result<()> {
    let reject = |reason: &str| Error::InvalidVersion {
        version: candidate.to_owned(),
        reason: reason.to_owned(),
    };

    if candidate.is_empty() {
        return Err(reject("version is empty"));
    }
    let mut chars = candidate.chars();
    let first = chars.next().unwrap_or_default();
    if !first.is_ascii_digit() {
        return Err(reject("version must start with a digit"));
    }
    for ch in chars {
        if !(ch.is_ascii_alphanumeric() || matches!(ch, '.' | '+' | '~')) {
            return Err(reject(&format!(
                "version contains `{ch}`; only alphanumerics, `.`, `+` and `~` are allowed"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapped(input: &str) -> MappedVersion {
        let spec = VersionSpec::parse(input).expect("test input should parse");
        MappedVersion::new(&spec, None).expect("test input should map")
    }

    #[test]
    fn release_version_passes_through() {
        let m = mapped("1.2.3");
        assert_eq!(m.deb(), "1.2.3");
        assert_eq!(m.rpm_version(), "1.2.3");
        assert_eq!(m.rpm_release(), "1");
    }

    #[test]
    fn prerelease_uses_tilde() {
        let m = mapped("1.2.3-beta.1");
        assert_eq!(m.deb(), "1.2.3~beta.1");
        assert_eq!(m.rpm_version(), "1.2.3~beta.1");
    }

    #[test]
    fn build_metadata_uses_plus() {
        assert_eq!(mapped("1.2.3+build.5").deb(), "1.2.3+build.5");
    }

    #[test]
    fn prerelease_and_build_metadata_combine() {
        assert_eq!(mapped("1.2.3-rc.1+build.5").deb(), "1.2.3~rc.1+build.5");
    }

    #[test]
    fn epoch_prefixes_deb_but_not_rpm() {
        let spec = VersionSpec::parse("1.2.3").unwrap();
        let m = MappedVersion::new(&spec, Some(1)).unwrap();
        assert_eq!(m.deb(), "1:1.2.3");
        assert_eq!(m.rpm_version(), "1.2.3");
        assert_eq!(m.epoch(), Some(1));
    }

    #[test]
    fn non_semver_but_valid_is_kept_literal() {
        let spec = VersionSpec::parse("1.2.3~rc1").unwrap();
        assert!(spec.is_literal());
        assert_eq!(MappedVersion::new(&spec, None).unwrap().deb(), "1.2.3~rc1");
    }

    #[test]
    fn version_not_starting_with_digit_is_rejected() {
        assert!(matches!(
            VersionSpec::parse("v1.2.3"),
            Err(Error::InvalidVersion { .. })
        ));
    }

    #[test]
    fn hyphen_never_survives_into_a_mapped_version() {
        // `1.2.3-1` is valid semver (pre-release `1`); the hyphen must not survive, or it
        // becomes a Debian revision sorting above the plain release.
        assert_eq!(mapped("1.2.3-1").deb(), "1.2.3~1");
        for input in ["1.2.3-1", "1.2.3-beta.1", "1.2.3-rc.1+build.5"] {
            assert!(
                !mapped(input).deb().contains('-'),
                "`{input}` mapped to a version still containing `-`"
            );
        }
    }

    #[test]
    fn hyphen_inside_a_prerelease_identifier_is_rewritten_not_rejected() {
        // semver allows `-` inside an identifier; Debian and RPM cannot carry it.
        let m = mapped("2.0.0-experimental-build");
        assert_eq!(m.deb(), "2.0.0~experimental.build");
        assert!(
            m.hyphens_rewritten(),
            "caller must be able to warn about the rewrite"
        );
    }

    #[test]
    fn hyphen_inside_build_metadata_is_rewritten() {
        // semver.org's own build-metadata example shape.
        let m = mapped("1.0.0-alpha+exp-sha.5114f85");
        assert_eq!(m.deb(), "1.0.0~alpha+exp.sha.5114f85");
        assert!(m.hyphens_rewritten());
    }

    #[test]
    fn versions_without_embedded_hyphens_report_no_rewrite() {
        assert!(!mapped("1.2.3-beta.1").hyphens_rewritten());
        assert!(!mapped("1.2.3").hyphens_rewritten());
    }

    #[test]
    fn mapping_error_names_the_version_as_supplied() {
        // Only reachable for a Literal, and no Literal passes the entry check yet fails once
        // an epoch is applied, so assert on the entry check: the message quotes the input.
        let err = VersionSpec::parse("1.2.3-really~odd-1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("1.2.3-really~odd-1"), "{err}");
    }

    #[test]
    fn literal_containing_a_hyphen_is_rejected() {
        // A bare revision separator is what this module exists to keep out.
        assert!(matches!(
            VersionSpec::parse("1.2.3-really~odd-1"),
            Err(Error::InvalidVersion { .. })
        ));
    }

    #[test]
    fn empty_version_is_rejected() {
        assert!(VersionSpec::parse("").is_err());
    }

    #[test]
    fn error_message_names_the_input() {
        let err = VersionSpec::parse("nope").unwrap_err().to_string();
        assert!(err.contains("nope"), "error should quote the input: {err}");
    }

    /// Checked against `debversion`, an independent implementation of policy 5.6.12; runs
    /// everywhere, unlike the `dpkg` test, and proves what we emit parses.
    #[test]
    fn mapped_versions_parse_and_sort_correctly_per_debversion() {
        use debversion::Version as DebVersion;

        let cases = [
            ("1.2.3-beta.1", "1.2.3"),
            ("1.2.3-beta.1", "1.2.3-rc.1"),
            ("1.2.3-rc.1+build.5", "1.2.3"),
            ("2.0.0-experimental-build", "2.0.0"),
            ("1.0.0-alpha+exp-sha.5114f85", "1.0.0"),
        ];
        for (lower, higher) in cases {
            let lo: DebVersion = mapped(lower)
                .deb()
                .parse()
                .unwrap_or_else(|e| panic!("`{lower}` mapped to an unparseable version: {e}"));
            let hi: DebVersion = mapped(higher)
                .deb()
                .parse()
                .unwrap_or_else(|e| panic!("`{higher}` mapped to an unparseable version: {e}"));
            assert!(
                lo < hi,
                "expected {lower} ({lo}) to sort below {higher} ({hi})"
            );
        }
    }

    /// The unmapped npm spelling sorts the wrong way: the defect this module fixes.
    #[test]
    fn unmapped_npm_prerelease_sorts_above_its_release() {
        use debversion::Version as DebVersion;
        let pre: DebVersion = "1.2.3-beta.1".parse().expect("valid debian version");
        let rel: DebVersion = "1.2.3".parse().expect("valid debian version");
        assert!(
            pre > rel,
            "if this ever fails, the tilde mapping is no longer needed"
        );
    }

    /// Ordering against the real `dpkg`. Skipped with a notice when it is unavailable; a
    /// silent pass on such a host would be worse than no test.
    #[test]
    fn prerelease_sorts_below_release_according_to_dpkg() {
        let Ok(probe) = std::process::Command::new("dpkg")
            .args(["--compare-versions", "1", "lt", "2"])
            .status()
        else {
            eprintln!("SKIP: `dpkg` not on PATH, ordering not verified against the real tool");
            return;
        };
        assert!(
            probe.success(),
            "dpkg present but the probe comparison failed"
        );

        let cases = [
            ("1.2.3-beta.1", "1.2.3"),
            ("1.2.3-beta.1", "1.2.3-rc.1"),
            ("1.2.3-rc.1+build.5", "1.2.3"),
        ];
        for (lower, higher) in cases {
            let lo = mapped(lower);
            let hi = mapped(higher);
            let status = std::process::Command::new("dpkg")
                .args(["--compare-versions", lo.deb(), "lt", hi.deb()])
                .status()
                .expect("dpkg was available a moment ago");
            assert!(
                status.success(),
                "expected {} ({}) to sort below {} ({})",
                lower,
                lo.deb(),
                higher,
                hi.deb()
            );
        }
    }
}
