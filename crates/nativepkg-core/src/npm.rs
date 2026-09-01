//! The npm side of the input: deserialisation of a Node.js project's `package.json`.
//!
//! Everything ecosystem-specific about the input lives here — the manifest shape and the
//! settings the tool reads from its `nativepkg` key. The rest of the core works from the
//! format-agnostic plan these produce.
//!
//! Read once into typed structures, with every enumerated field a real enum, so validation
//! has somewhere to live. Unknown top-level keys are ignored; unknown settings keys are kept
//! and reported.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// A project manifest. Unknown fields are ignored: a `package.json` is full of keys this tool
/// has no opinion about.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Manifest {
    pub name: Option<String>,
    pub version: Option<String>,
    pub description: Option<String>,
    pub author: Option<Author>,
    /// The authority on which directories may be linked into `node_modules`; inferring
    /// "anything inside the project" would let an untrusted dependency reach arbitrary files.
    pub workspaces: Option<Workspaces>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    pub nativepkg: Option<Settings>,
}

impl Manifest {
    /// Reads and parses a manifest. A parse error keeps the `serde_json` source, which
    /// carries the line and column.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
        serde_json::from_str(&raw).map_err(|source| Error::ManifestParse {
            path: path.to_path_buf(),
            source,
        })
    }
}

/// The `author` field: either `"Name <email>"` or `{name, email}`; extra keys such as `url`
/// are ignored. (bash rendered the object form as raw JSON into `Maintainer:`.)
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Author {
    Text(String),
    Structured { name: String, email: Option<String> },
}

impl Author {
    /// Renders the author as a Debian/RPM maintainer field.
    #[must_use]
    pub fn to_maintainer(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Structured {
                name,
                email: Some(email),
            } => format!("{name} <{email}>"),
            Self::Structured { name, email: None } => name.clone(),
        }
    }
}

/// The `workspaces` field: a list of patterns, or the Yarn-style `{ packages: [...] }`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Workspaces {
    Patterns(Vec<String>),
    Object { packages: Vec<String> },
}

impl Workspaces {
    #[must_use]
    pub fn patterns(&self) -> &[String] {
        match self {
            Self::Patterns(p) | Self::Object { packages: p } => p,
        }
    }

    /// Directory prefixes with any glob suffix removed: `packages/*` yields `packages`. A
    /// pattern for the project root itself (`*`) is dropped: admitting the root is the
    /// reach-through this exists to prevent.
    #[must_use]
    pub fn directory_prefixes(&self) -> Vec<String> {
        self.patterns()
            .iter()
            .filter_map(|pattern| {
                let cut = pattern.find(['*', '?', '[', '{']).unwrap_or(pattern.len());
                let prefix = pattern[..cut].trim_end_matches('/').trim();
                if prefix.is_empty() || prefix == "." {
                    None
                } else {
                    Some(prefix.to_owned())
                }
            })
            .collect()
    }
}

/// Which init system the generated package integrates with.
// `Serialize` only because `Settings` derives it (see `SETTING_KEYS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InitSystem {
    /// Detect at install time. The historical default.
    #[default]
    Auto,
    Upstart,
    Systemd,
    Sysv,
    None,
}

/// How runtime dependencies reach the installed package.
// `Serialize` only because `Settings` derives it (see `SETTING_KEYS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallStrategy {
    /// Vendor `node_modules` when present, otherwise install at unpack time.
    #[default]
    Auto,
    /// Vendor `node_modules` at build time.
    Copy,
    /// Install from the registry at package install time.
    NpmInstall,
}

/// Entry points into the packaged application.
// `Serialize` only because `Settings` derives it (see `SETTING_KEYS`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Entrypoints {
    /// Run by the service unit.
    pub daemon: Option<String>,
    /// Run by the `/usr/bin` wrapper. Defaults to the daemon entrypoint.
    pub cli: Option<String>,
}

/// User-supplied template overrides, each a path to a replacement template.
// `Serialize` only because `Settings` derives it (see `SETTING_KEYS`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Templates {
    pub control: Option<String>,
    pub executable: Option<String>,
    pub preinst: Option<String>,
    pub postinst: Option<String>,
    pub postrm: Option<String>,
    pub prerm: Option<String>,
    pub systemd_service: Option<String>,
    pub upstart_conf: Option<String>,
    pub sysv_init: Option<String>,
    pub default_variables: Option<String>,
}

/// Every key the `nativepkg` object accepts; what `--list-json-overrides` prints. A test
/// asserts it equals the fields of `Settings`, so a setting cannot be added without being
/// published.
pub const SETTING_KEYS: &[&str] = &[
    "package_name",
    "version",
    "epoch",
    "description",
    "maintainer",
    "homepage",
    "license",
    "architecture",
    "dependencies",
    "install_dir",
    "user",
    "group",
    "executable_name",
    "output_deb_name",
    "extra_files",
    "triggers_file",
    "init",
    "install_strategy",
    "install_command",
    "install_binary",
    "entrypoints",
    "templates",
];

/// The settings object, read from the `nativepkg` key.
/// Every field is optional: this is one layer in a precedence chain.
// `Serialize` exists so a test can read the field names and compare them with `SETTING_KEYS`;
// a hand-typed count once let a new field go unpublished with every test passing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    pub package_name: Option<String>,
    pub version: Option<String>,
    /// For when a version scheme change breaks ordering.
    pub epoch: Option<u32>,
    pub description: Option<String>,
    pub maintainer: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    pub architecture: Option<String>,
    pub dependencies: Option<String>,
    pub install_dir: Option<String>,
    pub user: Option<String>,
    pub group: Option<String>,
    pub executable_name: Option<String>,
    pub output_deb_name: Option<String>,
    /// Directory of files copied verbatim to the filesystem root.
    pub extra_files: Option<String>,
    /// A dpkg `triggers` control file, relative to the project root, shipped verbatim as
    /// cargo-deb does. Debian only; the other formats ignore it.
    pub triggers_file: Option<String>,
    pub init: Option<InitSystem>,
    pub install_strategy: Option<InstallStrategy>,
    /// Overrides the install-at-unpack command; otherwise the detected manager decides.
    pub install_command: Option<String>,
    /// Overrides the binary the install command is guarded on; otherwise derived.
    pub install_binary: Option<String>,
    pub entrypoints: Option<Entrypoints>,
    pub templates: Option<Templates>,

    /// Keys that are not settings, kept so a typo (`instal_dir`) is reported instead of
    /// silently ignored, which is serde's default.
    ///
    /// Must never hold a key equal to a named field: `#[serde(flatten)]` on a map does not
    /// deduplicate, so a colliding key would serialise as duplicate JSON keys.
    #[serde(flatten)]
    pub unknown: std::collections::BTreeMap<String, serde_json::Value>,
}

impl Settings {
    /// Reads a `.nativepkg` file: the settings a `nativepkg` object in package.json would carry,
    /// standing alone so a project needs no package.json.
    ///
    /// # Errors
    ///
    /// [`Error::Io`](crate::Error::Io) when the file cannot be read,
    /// [`Error::ManifestParse`](crate::Error::ManifestParse) when it is not valid JSON.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
        serde_json::from_str(&raw).map_err(|source| Error::ManifestParse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Unknown keys, sorted, each with the nearest real key when one is close enough.
    #[must_use]
    pub fn unknown_keys(&self) -> Vec<(String, Option<&'static str>)> {
        self.unknown
            .keys()
            .map(|key| (key.clone(), nearest_setting(key)))
            .collect()
    }
}

/// The published setting closest to `key`. Bounded so an unrelated key suggests nothing: a
/// wrong suggestion sends the reader looking in the wrong place.
fn nearest_setting(key: &str) -> Option<&'static str> {
    let limit = match key.chars().count() {
        0..=4 => 1,
        5..=8 => 2,
        _ => 3,
    };

    SETTING_KEYS
        .iter()
        .map(|candidate| (*candidate, distance(key, candidate)))
        .filter(|(_, d)| *d <= limit)
        .min_by_key(|(_, d)| *d)
        .map(|(candidate, _)| candidate)
}

/// Levenshtein distance between two ASCII-ish keys.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];

    for (i, ca) in a.chars().enumerate() {
        current[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(ca != *cb);
            current[j + 1] = substitution.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        core::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

/// The executable a command runs, skipping leading `KEY=value` environment assignments — so
/// `YARN_ENABLE_SCRIPTS=false yarn ...` yields `yarn`. `None` for an empty command.
#[must_use]
pub fn command_binary(command: &str) -> Option<&str> {
    command
        .split_whitespace()
        .find(|token| !token.contains('='))
}

#[cfg(test)]
mod tests {
    /// Serialising a default value is what makes this a real check: deserialising each key
    /// proves nothing (`#[serde(flatten)]` accepts anything), and a hand-typed count let a new
    /// field go unpublished with every test passing.
    #[test]
    fn the_guard_binary_skips_environment_assignments() {
        assert_eq!(command_binary("pnpm install --prod"), Some("pnpm"));
        assert_eq!(
            command_binary("YARN_ENABLE_SCRIPTS=false yarn workspaces focus"),
            Some("yarn")
        );
        assert_eq!(command_binary("  npm ci  "), Some("npm"));
        assert_eq!(command_binary("FOO=1 BAR=2"), None);
        assert_eq!(command_binary(""), None);
    }

    #[test]
    fn the_published_setting_keys_are_exactly_the_fields() {
        let value = serde_json::to_value(Settings::default()).expect("Settings serialises");
        let object = value.as_object().expect("a JSON object");

        let mut actual: Vec<&str> = object.keys().map(String::as_str).collect();
        actual.sort_unstable();
        let mut published = SETTING_KEYS.to_vec();
        published.sort_unstable();

        assert_eq!(
            actual, published,
            "`SETTING_KEYS` and the fields of `Settings` have drifted apart; publish the new \
             setting, or remove the entry for the one that is gone"
        );
    }

    #[test]
    fn an_unknown_setting_key_is_captured_rather_than_ignored() {
        let settings: Settings = serde_json::from_str(r#"{"not_a_setting": 1}"#).expect("parses");
        assert_eq!(
            settings.unknown_keys(),
            vec![("not_a_setting".to_owned(), None)],
            "unknown keys must be visible, not silently dropped"
        );
    }

    #[test]
    fn a_near_miss_suggests_the_setting_it_resembles() {
        let settings: Settings = serde_json::from_str(
            r#"{"instal_dir": "/opt", "packge_name": "x", "totally-unrelated": 1}"#,
        )
        .expect("parses");
        let found = settings.unknown_keys();

        assert!(
            found.contains(&("instal_dir".to_owned(), Some("install_dir"))),
            "{found:?}"
        );
        assert!(
            found.contains(&("packge_name".to_owned(), Some("package_name"))),
            "{found:?}"
        );

        // An unrelated key suggests nothing: a wrong suggestion is worse than silence.
        assert!(
            found.contains(&("totally-unrelated".to_owned(), None)),
            "{found:?}"
        );
    }

    #[test]
    fn a_real_setting_is_not_reported() {
        for key in SETTING_KEYS {
            let settings: Settings = serde_json::from_str(&format!("{{\"{key}\": null}}"))
                .unwrap_or_else(|e| panic!("`{key}`: {e}"));
            assert!(
                settings.unknown_keys().is_empty(),
                "`{key}` is a real setting but was reported as unknown"
            );
        }
    }

    use super::*;

    fn parse(json: &str) -> Manifest {
        serde_json::from_str(json).expect("test fixture should deserialise")
    }

    #[test]
    fn unknown_fields_are_tolerated() {
        let m = parse(r#"{"name":"a","scripts":{"test":"x"},"devDependencies":{"y":"1"}}"#);
        assert_eq!(m.name.as_deref(), Some("a"));
    }

    #[test]
    fn string_author_becomes_the_maintainer_verbatim() {
        let m = parse(r#"{"author":"Ivan Smirnov <i@example.com>"}"#);
        assert_eq!(
            m.author.unwrap().to_maintainer(),
            "Ivan Smirnov <i@example.com>"
        );
    }

    #[test]
    fn object_author_is_rendered_with_angle_brackets() {
        let m = parse(
            r#"{"author":{"name":"Ivan Smirnov","email":"i@example.com","url":"https://x"}}"#,
        );
        assert_eq!(
            m.author.unwrap().to_maintainer(),
            "Ivan Smirnov <i@example.com>"
        );
    }

    #[test]
    fn object_author_without_email_renders_the_name_only() {
        let m = parse(r#"{"author":{"name":"Ivan Smirnov"}}"#);
        assert_eq!(m.author.unwrap().to_maintainer(), "Ivan Smirnov");
    }

    #[test]
    fn absent_author_is_none() {
        assert!(parse(r#"{"name":"a"}"#).author.is_none());
    }

    #[test]
    fn init_system_deserialises_from_lowercase() {
        let m = parse(r#"{"nativepkg":{"init":"systemd"}}"#);
        assert_eq!(m.nativepkg.unwrap().init, Some(InitSystem::Systemd));
    }

    #[test]
    fn install_strategy_deserialises_from_kebab_case() {
        let m = parse(r#"{"nativepkg":{"install_strategy":"npm-install"}}"#);
        assert_eq!(
            m.nativepkg.unwrap().install_strategy,
            Some(InstallStrategy::NpmInstall)
        );
    }

    #[test]
    fn unknown_init_system_is_a_parse_error() {
        let err = serde_json::from_str::<Manifest>(r#"{"nativepkg":{"init":"launchd"}}"#);
        assert!(
            err.is_err(),
            "an unknown init system must not silently default"
        );
    }

    #[test]
    fn missing_file_reports_the_path() {
        let err = Manifest::from_path("/nonexistent/package.json").unwrap_err();
        assert!(err.to_string().contains("/nonexistent/package.json"));
    }

    #[test]
    fn malformed_json_reports_a_position() {
        // Unique directory per test so parallel runs cannot race on the same path.
        let dir = std::env::temp_dir().join("nativepkg-malformed-json-test");
        std::fs::create_dir_all(&dir).expect("temp dir should be creatable");
        let path = dir.join("package.json");
        std::fs::write(&path, "{ not json").expect("fixture should be writable");

        let err = Manifest::from_path(&path).expect_err("malformed JSON must not parse");
        std::fs::remove_file(&path).ok();

        let Error::ManifestParse { source, .. } = &err else {
            panic!("expected a parse error, got {err:?}");
        };
        // The position lives on the `#[source]` error, not flattened into our message.
        assert_eq!(source.line(), 1, "parser error should carry a line");
        assert!(source.column() > 0, "parser error should carry a column");
        assert!(
            err.to_string().contains("package.json"),
            "our own message should name the file: {err}"
        );
    }
}
