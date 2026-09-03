//! Target architecture, parsed once and rendered per format: Debian says `amd64` and `all`
//! where RPM says `x86_64` and `noarch`. bash carried a free string defaulted to `all`, so a
//! typo surfaced — if at all — inside the packaging tool.

use serde::{Deserialize, Serialize};

use crate::core::{Error, Result};

/// A target architecture. Both the Debian and the GNU spelling are accepted on input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Architecture {
    /// Architecture-independent content.
    Any,
    Amd64,
    Arm64,
    I386,
    Armhf,
    Ppc64le,
    S390x,
    Riscv64,
}

impl Architecture {
    /// Used by both parsing and the error message, so the two cannot disagree.
    const SPELLINGS: &'static [(&'static str, Self)] = &[
        ("all", Self::Any),
        ("any", Self::Any),
        ("noarch", Self::Any),
        ("amd64", Self::Amd64),
        ("x86_64", Self::Amd64),
        ("arm64", Self::Arm64),
        ("aarch64", Self::Arm64),
        ("i386", Self::I386),
        ("i686", Self::I386),
        ("x86", Self::I386),
        ("armhf", Self::Armhf),
        ("armv7", Self::Armhf),
        ("armv7hl", Self::Armhf),
        ("ppc64el", Self::Ppc64le),
        ("ppc64le", Self::Ppc64le),
        ("s390x", Self::S390x),
        ("riscv64", Self::Riscv64),
    ];

    /// Parses any accepted spelling. Never silently defaulted: that would produce a package
    /// claiming to run somewhere it cannot.
    pub fn parse(input: &str) -> Result<Self> {
        let lowered = input.to_ascii_lowercase();
        Self::SPELLINGS
            .iter()
            .find(|(spelling, _)| *spelling == lowered)
            .map(|(_, arch)| *arch)
            .ok_or_else(|| {
                let accepted: Vec<&str> = Self::SPELLINGS.iter().map(|(s, _)| *s).collect();
                Error::manifest(format!(
                    "unknown architecture `{input}`; accepted spellings: {}",
                    accepted.join(", ")
                ))
            })
    }

    /// The Debian spelling, for a `control` file's `Architecture` field.
    #[must_use]
    pub fn deb(self) -> &'static str {
        match self {
            Self::Any => "all",
            Self::Amd64 => "amd64",
            Self::Arm64 => "arm64",
            Self::I386 => "i386",
            Self::Armhf => "armhf",
            Self::Ppc64le => "ppc64el",
            Self::S390x => "s390x",
            Self::Riscv64 => "riscv64",
        }
    }

    /// The RPM spelling, for the header's architecture tag.
    #[must_use]
    pub fn rpm(self) -> &'static str {
        match self {
            Self::Any => "noarch",
            Self::Amd64 => "x86_64",
            Self::Arm64 => "aarch64",
            Self::I386 => "i686",
            Self::Armhf => "armv7hl",
            Self::Ppc64le => "ppc64le",
            Self::S390x => "s390x",
            Self::Riscv64 => "riscv64",
        }
    }

    /// The Arch Linux spelling, for `.PKGINFO`'s `arch` field.
    #[must_use]
    pub fn arch_linux(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Amd64 => "x86_64",
            Self::Arm64 => "aarch64",
            Self::I386 => "i686",
            Self::Armhf => "armv7h",
            Self::Ppc64le => "powerpc64le",
            Self::S390x => "s390x",
            Self::Riscv64 => "riscv64",
        }
    }

    #[must_use]
    pub fn is_any(self) -> bool {
        self == Self::Any
    }
}

impl Default for Architecture {
    /// Matches the bash implementation's default.
    fn default() -> Self {
        Self::Any
    }
}

// `Display` is deliberately not implemented: it would have to pick one format's spelling and
// then look format-neutral at every call site. Callers say which: `deb()`, `rpm()`,
// `arch_linux()`.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debian_and_gnu_spellings_agree() {
        for (a, b) in [
            ("amd64", "x86_64"),
            ("arm64", "aarch64"),
            ("i386", "i686"),
            ("armhf", "armv7hl"),
            ("ppc64el", "ppc64le"),
            ("all", "noarch"),
        ] {
            assert_eq!(
                Architecture::parse(a).unwrap(),
                Architecture::parse(b).unwrap(),
                "`{a}` and `{b}` should denote the same architecture"
            );
        }
    }

    #[test]
    fn architecture_independent_renders_per_format() {
        let any = Architecture::parse("all").unwrap();
        assert_eq!(any.deb(), "all");
        assert_eq!(any.rpm(), "noarch");
        assert_eq!(any.arch_linux(), "any");
        assert!(any.is_any());
    }

    #[test]
    fn amd64_renders_per_format() {
        let a = Architecture::parse("x86_64").unwrap();
        assert_eq!(a.deb(), "amd64");
        assert_eq!(a.rpm(), "x86_64");
        assert_eq!(a.arch_linux(), "x86_64");
    }

    #[test]
    fn parsing_is_case_insensitive() {
        assert_eq!(Architecture::parse("AMD64").unwrap(), Architecture::Amd64);
    }

    #[test]
    fn unknown_architecture_is_rejected_not_defaulted() {
        let err = Architecture::parse("sparc64").unwrap_err().to_string();
        assert!(err.contains("sparc64"), "{err}");
        assert!(
            err.contains("amd64"),
            "error should list accepted spellings: {err}"
        );
    }

    #[test]
    fn every_spelling_round_trips_to_a_renderable_value() {
        for (spelling, _) in Architecture::SPELLINGS {
            let a = Architecture::parse(spelling).unwrap();
            assert!(!a.deb().is_empty());
            assert!(!a.rpm().is_empty());
            assert!(!a.arch_linux().is_empty());
        }
    }

    #[test]
    fn default_matches_the_bash_implementation() {
        assert_eq!(Architecture::default().deb(), "all");
    }

    #[test]
    fn serde_round_trip() {
        let a = Architecture::Ppc64le;
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(serde_json::from_str::<Architecture>(&json).unwrap(), a);
    }
}
