//! Names that are valid by construction: the only way to obtain one is through a validating
//! constructor, so no downstream code builds a path or a script from a name the packaging
//! tools will reject. (bash concatenated `@acme/app` straight into a path and left a stray
//! `@acme/` directory behind.)

use core::fmt;
use core::ops::Deref;
use std::path::Path;

use crate::{Error, Result};

/// A package name satisfying `^[a-z0-9][a-z0-9+.-]+$`. [`PackageName::normalize`] is
/// forgiving, for names from `package.json`; [`PackageName::parse_strict`] is exact, for
/// names the user typed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PackageName(String);

impl PackageName {
    /// Minimum length dpkg accepts. The grammar's `+` quantifier implies at least two.
    const MIN_LEN: usize = 2;

    /// Validates without transforming: if the user typed it, they meant it.
    pub fn parse_strict(input: &str) -> Result<Self> {
        Self::validate(input)?;
        Ok(Self(input.to_owned()))
    }

    /// Drops a leading `@`, maps `/` and `_` to `-`, lowercases, collapses `-` runs and trims.
    /// Callers compare the result with the input and warn, because flattening a scope is
    /// lossy: `@a/b-c` and `@a-b/c` both become `a-b-c`.
    pub fn normalize(input: &str) -> Result<Self> {
        let stripped = input.strip_prefix('@').unwrap_or(input);

        let mut out = String::with_capacity(stripped.len());
        for ch in stripped.chars() {
            match ch {
                '/' | '_' | '-' | ' ' if !out.ends_with('-') => out.push('-'),
                '/' | '_' | '-' | ' ' => {}
                c if c.is_ascii_alphanumeric() => out.push(c.to_ascii_lowercase()),
                '+' | '.' => out.push(ch),
                // Dropped rather than transliterated: a wrong guess is worse than a shorter
                // name the user can override.
                _ => {}
            }
        }

        let trimmed = out.trim_matches('-');
        Self::validate(trimmed).map_err(|_| Error::InvalidPackageName {
            name: input.to_owned(),
            reason: format!(
                "normalised to `{trimmed}`, which is not a valid package name; \
                 pass an explicit name to override"
            ),
        })?;
        Ok(Self(trimmed.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }

    fn validate(candidate: &str) -> Result<()> {
        let reject = |reason: &str| Error::InvalidPackageName {
            name: candidate.to_owned(),
            reason: reason.to_owned(),
        };

        if candidate.is_empty() {
            return Err(reject("name is empty"));
        }
        if candidate.len() < Self::MIN_LEN {
            return Err(reject("name must be at least two characters"));
        }

        let mut chars = candidate.chars();
        // `is_empty` above guarantees a first character exists.
        let first = chars.next().unwrap_or_default();
        if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
            return Err(reject(
                "name must start with an ASCII lowercase letter or a digit",
            ));
        }
        for ch in chars {
            if !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '+' | '-' | '.')) {
                return Err(reject(&format!(
                    "name contains `{ch}`; only lowercase letters, digits, `+`, `-` and `.` are allowed"
                )));
            }
        }
        Ok(())
    }
}

impl fmt::Display for PackageName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for PackageName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_name_loses_the_slash() {
        assert_eq!(
            PackageName::normalize("@acme/probe-app").unwrap().as_str(),
            "acme-probe-app"
        );
    }

    #[test]
    fn uppercase_is_lowered() {
        assert_eq!(PackageName::normalize("MyApp").unwrap().as_str(), "myapp");
    }

    #[test]
    fn underscores_become_hyphens() {
        assert_eq!(
            PackageName::normalize("My_App_Thing").unwrap().as_str(),
            "my-app-thing"
        );
    }

    #[test]
    fn repeated_separators_collapse() {
        assert_eq!(PackageName::normalize("a__--//b").unwrap().as_str(), "a-b");
    }

    #[test]
    fn already_valid_name_is_unchanged() {
        let input = "simple";
        assert_eq!(PackageName::normalize(input).unwrap().as_str(), input);
    }

    #[test]
    fn plus_and_dot_survive_normalisation() {
        assert_eq!(
            PackageName::normalize("lib.foo+bar").unwrap().as_str(),
            "lib.foo+bar"
        );
    }

    #[test]
    fn separator_only_input_is_rejected() {
        let err = PackageName::normalize("___").unwrap_err();
        assert!(matches!(err, Error::InvalidPackageName { .. }));
    }

    #[test]
    fn empty_input_is_rejected() {
        assert!(PackageName::normalize("").is_err());
    }

    #[test]
    fn name_starting_with_punctuation_is_rejected() {
        // `.` is legal inside a name but not as the first character.
        assert!(PackageName::normalize(".hidden").is_err());
    }

    #[test]
    fn single_character_is_rejected() {
        assert!(PackageName::normalize("x").is_err());
    }

    #[test]
    fn strict_parse_rejects_what_normalize_would_fix() {
        assert!(PackageName::parse_strict("MyApp").is_err());
        assert!(PackageName::parse_strict("@acme/app").is_err());
    }

    #[test]
    fn strict_parse_accepts_a_valid_name() {
        assert_eq!(
            PackageName::parse_strict("my-app").unwrap().as_str(),
            "my-app"
        );
    }

    #[test]
    fn every_normalised_name_satisfies_the_grammar() {
        for input in [
            "@acme/probe-app",
            "MyApp",
            "My_App_Thing",
            "a__--//b",
            "lib.foo+bar",
            "UPPER",
            "with spaces",
        ] {
            let name = PackageName::normalize(input).unwrap();
            assert!(
                PackageName::parse_strict(name.as_str()).is_ok(),
                "normalising `{input}` produced `{name}`, which fails strict validation"
            );
        }
    }

    #[test]
    fn error_message_names_the_original_input() {
        let err = PackageName::normalize("___").unwrap_err().to_string();
        assert!(err.contains("___"), "error should quote the input: {err}");
    }
}

/// A Unix account name accepted by `adduser --system`, which Debian Policy §9.2.2 directs
/// packages to use for service accounts. bash checked only the length, so `0ad` (a real
/// package name) reached `useradd` and failed at install time; and `--user` is interpolated
/// into maintainer scripts and units, so the character class is enforced here.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UnixName(String);

impl UnixName {
    /// Not a hard `useradd` limit (it documents 256); 32 is the `utmp` convention, kept as the
    /// conservative choice.
    pub const MAX_LEN: usize = 32;

    /// Validates `input` without transforming it: `^[a-z_][a-z0-9_-]*$`, at most
    /// [`UnixName::MAX_LEN`]. A lowercased subset of `adduser --system`'s `SYS_NAME_REGEX`;
    /// plain `adduser` and raw `useradd` are not the reference, since only `adduser --system`
    /// admits the leading underscore [`UnixName::derive_from`] relies on.
    pub fn parse_strict(kind: &'static str, input: &str) -> Result<Self> {
        Self::validate(kind, input)?;
        Ok(Self(input.to_owned()))
    }

    /// Lowercases, maps anything outside the grammar to `-`, collapses, trims, truncates to
    /// [`UnixName::MAX_LEN`], and prefixes `_` when the result would start with a digit — the
    /// Debian convention for system accounts.
    pub fn derive_from(kind: &'static str, source: &str) -> Result<Self> {
        let mut out = String::with_capacity(source.len());
        for ch in source.chars() {
            match ch {
                c if c.is_ascii_alphanumeric() => out.push(c.to_ascii_lowercase()),
                '_' => out.push('_'),
                _ if !out.ends_with('-') => out.push('-'),
                _ => {}
            }
        }

        let mut derived = out.trim_matches('-').to_owned();
        if derived.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            derived.insert(0, '_');
        }
        derived.truncate(Self::MAX_LEN);
        // Truncation can expose a trailing separator; trim again so the result is tidy.
        let derived = derived.trim_end_matches('-').to_owned();

        Self::validate(kind, &derived).map_err(|_| Error::InvalidUnixName {
            kind,
            name: source.to_owned(),
            reason: format!(
                "derived `{derived}`, which is not a valid {kind} name; \
                 set the {kind} explicitly to override"
            ),
        })?;
        Ok(Self(derived))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn validate(kind: &'static str, candidate: &str) -> Result<()> {
        let reject = |reason: &str| Error::InvalidUnixName {
            kind,
            name: candidate.to_owned(),
            reason: reason.to_owned(),
        };

        if candidate.is_empty() {
            return Err(reject("name is empty"));
        }
        if candidate.len() > Self::MAX_LEN {
            return Err(reject(&format!(
                "name is {} characters; the maximum is {}",
                candidate.len(),
                Self::MAX_LEN
            )));
        }

        let mut chars = candidate.chars();
        let first = chars.next().unwrap_or_default();
        if !(first.is_ascii_lowercase() || first == '_') {
            return Err(reject(
                "name must start with a lowercase letter or an underscore",
            ));
        }
        for ch in chars {
            if !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '_' | '-')) {
                return Err(reject(&format!(
                    "name contains `{ch}`; only lowercase letters, digits, `_` and `-` are allowed"
                )));
            }
        }
        Ok(())
    }
}

impl fmt::Display for UnixName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for UnixName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod unix_name_tests {
    use super::*;

    #[test]
    fn a_plain_name_is_accepted_unchanged() {
        assert_eq!(
            UnixName::parse_strict("user", "simple").unwrap().as_str(),
            "simple"
        );
    }

    #[test]
    fn package_names_starting_with_a_digit_get_an_underscore() {
        // `0ad` is a real Debian package; `useradd` refuses a leading digit.
        assert_eq!(
            UnixName::derive_from("user", "0ad").unwrap().as_str(),
            "_0ad"
        );
    }

    #[test]
    fn dots_and_pluses_valid_in_package_names_are_mapped_out() {
        assert_eq!(
            UnixName::derive_from("user", "lib.foo+bar")
                .unwrap()
                .as_str(),
            "lib-foo-bar"
        );
    }

    #[test]
    fn derivation_truncates_to_the_shadow_limit() {
        let long = "a".repeat(64);
        let derived = UnixName::derive_from("user", &long).unwrap();
        assert_eq!(derived.as_str().len(), UnixName::MAX_LEN);
    }

    #[test]
    fn strict_parse_rejects_a_leading_digit() {
        assert!(UnixName::parse_strict("user", "0ad").is_err());
    }

    #[test]
    fn strict_parse_rejects_shell_metacharacters() {
        // The injection case: this would otherwise reach a generated script and `useradd`.
        let err = UnixName::parse_strict("user", "bad user; rm -rf /").unwrap_err();
        assert!(matches!(err, Error::InvalidUnixName { .. }), "{err:?}");
    }

    #[test]
    fn strict_parse_rejects_uppercase_and_dots() {
        assert!(UnixName::parse_strict("user", "MyUser").is_err());
        assert!(UnixName::parse_strict("group", "my.group").is_err());
    }

    #[test]
    fn over_long_names_are_rejected() {
        assert!(UnixName::parse_strict("user", &"u".repeat(33)).is_err());
    }

    #[test]
    fn empty_and_separator_only_sources_are_rejected() {
        assert!(UnixName::derive_from("user", "").is_err());
        assert!(UnixName::derive_from("user", "...").is_err());
    }

    #[test]
    fn the_error_says_which_kind_it_was() {
        let err = UnixName::parse_strict("group", "Bad")
            .unwrap_err()
            .to_string();
        assert!(err.contains("group"), "{err}");
    }

    #[test]
    fn every_derived_name_satisfies_strict_validation() {
        for source in ["0ad", "lib.foo+bar", "acme-probe-app", "simple", "a", "_x"] {
            let derived = UnixName::derive_from("user", source).unwrap();
            assert!(
                UnixName::parse_strict("user", derived.as_str()).is_ok(),
                "deriving from `{source}` produced `{derived}`, which fails strict validation"
            );
        }
    }
}

const PATH_CHARS: &str = "letters, digits, `.`, `_`, `+` and `-`";

fn path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-')
}

fn check_segment(kind: &str, value: &str, segment: &str) -> Result<()> {
    if segment == "." || segment == ".." {
        return Err(Error::manifest(format!(
            "{kind} `{value}` contains `{segment}` as a path component"
        )));
    }
    if let Some(c) = segment.chars().find(|c| !path_char(*c)) {
        return Err(Error::manifest(format!(
            "{kind} `{value}` contains `{c}`; only {PATH_CHARS} and `/` are allowed"
        )));
    }
    Ok(())
}

/// Where a package installs: an absolute path of characters that need no quoting. It goes
/// unquoted into `ExecStart=` and the `/usr/bin` wrapper, and single-quoted into maintainer
/// scripts that run as root; the grammar is what makes that safe.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InstallDir(String);

impl InstallDir {
    /// Parses and normalises (`//` collapsed, no trailing slash).
    pub fn parse(input: &str) -> Result<Self> {
        if !input.starts_with('/') {
            return Err(Error::manifest(format!(
                "install_dir `{input}` must be an absolute path"
            )));
        }
        let segments: Vec<&str> = input.split('/').filter(|s| !s.is_empty()).collect();
        if segments.is_empty() {
            return Err(Error::manifest(
                "install_dir must not be the filesystem root".to_owned(),
            ));
        }
        for segment in &segments {
            check_segment("install_dir", input, segment)?;
        }
        Ok(Self(format!("/{}", segments.join("/"))))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }
}

impl Deref for InstallDir {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for InstallDir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The command placed on `/usr/bin`: one path component, so it cannot name a file anywhere
/// else — `../../etc/cron.d/x` once did — and made of characters that need no quoting.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExecutableName(String);

impl ExecutableName {
    pub fn parse(input: &str) -> Result<Self> {
        if input.is_empty() {
            return Err(Error::manifest("executable_name is empty".to_owned()));
        }
        if input.contains('/') {
            return Err(Error::manifest(format!(
                "executable_name `{input}` contains `/`; it is a single name placed on /usr/bin"
            )));
        }
        check_segment("executable_name", input, input)?;
        Ok(Self(input.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for ExecutableName {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ExecutableName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A script inside the shipped application, relative to its root. It lands unquoted in
/// `ExecStart=` and inside shell strings in init scripts, so it gets the path grammar plus `@`
/// for scoped-package layouts. A leading `./` is dropped.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntryPoint(String);

impl EntryPoint {
    pub fn parse(input: &str) -> Result<Self> {
        if input.starts_with('/') {
            return Err(Error::manifest(format!(
                "entrypoint `{input}` must be relative to the project"
            )));
        }
        let segments: Vec<&str> = input
            .split('/')
            .filter(|s| !s.is_empty() && *s != ".")
            .collect();
        if segments.is_empty() {
            return Err(Error::manifest(format!(
                "entrypoint `{input}` names no file"
            )));
        }
        for segment in &segments {
            if *segment == ".." {
                return Err(Error::manifest(format!(
                    "entrypoint `{input}` contains `..`; it must stay inside the project"
                )));
            }
            if let Some(c) = segment.chars().find(|c| !(path_char(*c) || *c == '@')) {
                return Err(Error::manifest(format!(
                    "entrypoint `{input}` contains `{c}`; only {PATH_CHARS}, `@` and `/` are allowed"
                )));
            }
        }
        Ok(Self(segments.join("/")))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for EntryPoint {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EntryPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod path_names {
    use super::*;

    #[test]
    fn install_dir_accepts_plain_absolute_paths_and_normalises_them() {
        assert_eq!(InstallDir::parse("/usr/lib").unwrap().as_str(), "/usr/lib");
        assert_eq!(InstallDir::parse("/opt//x/").unwrap().as_str(), "/opt/x");
        assert_eq!(
            InstallDir::parse("/srv/app-1.2_3+b").unwrap().as_str(),
            "/srv/app-1.2_3+b"
        );
    }

    #[test]
    fn install_dir_refuses_everything_a_shell_or_unit_file_would_misread() {
        for bad in [
            "usr/lib",
            "/",
            "/usr/lib/x'; touch /tmp/PWNED; y='",
            "/opt/my app",
            "/opt/$HOME",
            "/opt/a`b`",
            "/opt/../etc",
            "/opt/./x",
            "/opt/x\n",
        ] {
            assert!(InstallDir::parse(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn executable_name_is_one_safe_component() {
        assert!(ExecutableName::parse("hello").is_ok());
        assert!(ExecutableName::parse("my-tool.v2").is_ok());
        for bad in [
            "",
            "../../etc/cron.d/evil",
            "a/b",
            "..",
            ".",
            "a b",
            "a;b",
            "a$b",
        ] {
            assert!(
                ExecutableName::parse(bad).is_err(),
                "{bad:?} must be refused"
            );
        }
    }

    #[test]
    fn entrypoint_stays_inside_the_project_and_needs_no_quoting() {
        assert_eq!(
            EntryPoint::parse("./index.js").unwrap().as_str(),
            "index.js"
        );
        assert_eq!(
            EntryPoint::parse("bin/cli.js").unwrap().as_str(),
            "bin/cli.js"
        );
        assert!(EntryPoint::parse("node_modules/@scope/pkg/bin/x.js").is_ok());
        for bad in [
            "",
            "/etc/passwd",
            "../x.js",
            "index.js; touch /tmp/x",
            "my app.js",
            "a$(b).js",
            ".",
        ] {
            assert!(EntryPoint::parse(bad).is_err(), "{bad:?} must be refused");
        }
    }
}
