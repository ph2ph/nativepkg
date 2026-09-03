//! Rendering the files a package generates: `{{ name }}` substitution and nothing else.
//!
//! bash spliced values into a `sed` program, so a newline in a description aborted the build
//! and an unknown placeholder survived verbatim into scripts that run as root. There is no
//! expression language; `upon` was rejected because its unknown-variable error names neither
//! the variable nor a position, and naming the variable is the requirement.

use std::collections::BTreeMap;

use crate::core::name::EntryPoint;
use crate::core::{Error, Result};

const OPEN: &str = "{{";

const CLOSE: &str = "}}";

/// How much of an unterminated placeholder to quote back, in characters.
const CONTEXT_CHARS: usize = 32;

/// The values a template may reference.
#[derive(Debug, Clone, Default)]
pub struct Variables {
    values: BTreeMap<String, String>,
}

/// Every variable [`Variables::for_config`] defines; what `--list-tmp-vars` prints.
/// A test asserts it equals what `for_config` actually populates, so the two cannot drift.
const CANONICAL: &[&str] = &[
    "package_name",
    "package_version",
    "package_description",
    "package_description_shell",
    "package_maintainer",
    "package_maintainer_shell",
    "package_dependencies",
    "package_architecture",
    "executable_name",
    "install_dir",
    "user",
    "group",
    "init",
    "install_strategy",
    "install_binary",
    "install_command",
    "no_rebuild",
    "generator_version",
    "cli_entrypoint",
    "daemon_entrypoint",
];

impl Variables {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a variable.
    #[must_use]
    pub fn with(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.values.insert(name.into(), value.into());
        self
    }

    /// Looks a variable up.
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    /// Every current spelling, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.values.keys().map(String::as_str).collect()
    }

    /// Every accepted spelling including legacy aliases, sorted, each paired with its
    /// current spelling.
    #[must_use]
    pub fn all_spellings(&self) -> Vec<(&str, &str)> {
        let mut out: Vec<(&str, &str)> = self
            .values
            .keys()
            .map(|k| (k.as_str(), k.as_str()))
            .collect();
        out.sort_unstable();
        out
    }

    /// Every variable a template may use, current spellings only.
    #[must_use]
    pub fn vocabulary() -> Vec<&'static str> {
        CANONICAL.to_vec()
    }

    /// Every accepted spelling, as `(spelling, canonical)`; legacy aliases included.
    #[must_use]
    pub fn accepted_spellings() -> Vec<(&'static str, &'static str)> {
        let mut out: Vec<(&'static str, &'static str)> =
            CANONICAL.iter().map(|name| (*name, *name)).collect();
        out.sort_unstable();
        out
    }

    /// The full vocabulary for a resolved configuration.
    ///
    /// `version` and `architecture` come from the caller because only the backend knows which
    /// format's spelling it is writing: an RPM once rendered `MappedVersion::deb()` (wrong
    /// whenever an epoch is set) and `amd64` where its own header said `x86_64`.
    #[must_use]
    pub fn for_config(
        config: &crate::core::resolve::ResolvedConfig,
        generator_version: &str,
        version: &str,
        architecture: &str,
    ) -> Self {
        let init = match config.init {
            crate::core::npm::InitSystem::Auto => "auto",
            crate::core::npm::InitSystem::Upstart => "upstart",
            crate::core::npm::InitSystem::Systemd => "systemd",
            crate::core::npm::InitSystem::Sysv => "sysv",
            crate::core::npm::InitSystem::None => "none",
        };
        let strategy = match config.install_strategy {
            crate::core::npm::InstallStrategy::Auto => "auto",
            crate::core::npm::InstallStrategy::Copy => "copy",
            crate::core::npm::InstallStrategy::NpmInstall => "npm-install",
        };
        // Synopsis only: every template context is one line, and a second description line
        // once became an `ExecStartPre=` in the unit. The `_shell` forms go inside a
        // double-quoted shell string.
        let synopsis = config.description.lines().next().unwrap_or("").trim();
        Self::new()
            .with("package_name", config.package_name.as_str())
            .with("package_version", version)
            .with("package_description", synopsis)
            .with(
                "package_description_shell",
                crate::core::text::shell_double_quoted(synopsis),
            )
            .with("package_maintainer", &config.maintainer)
            .with(
                "package_maintainer_shell",
                crate::core::text::shell_double_quoted(&config.maintainer),
            )
            .with(
                "package_dependencies",
                config.dependencies.as_deref().unwrap_or_default(),
            )
            .with("package_architecture", architecture)
            .with("executable_name", config.executable_name.as_str())
            .with("install_dir", config.install_dir.as_str())
            .with("user", config.user.as_str())
            .with("group", config.group.as_str())
            .with("init", init)
            .with("install_strategy", strategy)
            .with("install_binary", &config.install_binary)
            .with("install_command", &config.install_command)
            // `--no-rebuild` is not plumbed through yet; `0` is bash's own default.
            .with("no_rebuild", "0")
            .with("generator_version", generator_version)
            .with(
                "cli_entrypoint",
                config
                    .cli_entrypoint
                    .as_ref()
                    .map_or("", EntryPoint::as_str),
            )
            .with(
                "daemon_entrypoint",
                config
                    .daemon_entrypoint
                    .as_ref()
                    .map_or("", EntryPoint::as_str),
            )
    }

    /// The known name closest to `unknown`, when one is close enough to be worth suggesting.
    fn closest(&self, unknown: &str) -> Option<&str> {
        // Roughly a third of the name's length catches transpositions and single slips
        // without proposing something unrelated.
        let budget = (unknown.len() / 3).max(1);
        self.values
            .keys()
            .map(|candidate| (edit_distance(unknown, candidate), candidate.as_str()))
            .filter(|(distance, _)| *distance <= budget)
            .min_by_key(|(distance, _)| *distance)
            .map(|(_, name)| name)
    }
}

/// Renders `source`, substituting every placeholder. `name` identifies the template in errors.
///
/// # Errors
///
/// [`Error::Template`] for an unterminated or empty placeholder, or an unknown variable — fatal
/// rather than passed through, since the alternative is shipping it inside a script that runs
/// as root. The message names the closest known spelling when there is one.
pub fn render(name: &str, source: &str, variables: &Variables) -> Result<String> {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;

    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + OPEN.len()..];

        let Some(end) = after_open.find(CLOSE) else {
            // By characters, not bytes: templates are user files, and a byte slice panics on a
            // straddled multi-byte character — inside the construction of this very error.
            let context: String = rest[start..].chars().take(CONTEXT_CHARS).collect();
            return Err(Error::Template {
                template: name.to_owned(),
                reason: format!(
                    "unterminated placeholder: `{context}` is never closed with `{CLOSE}`"
                ),
            });
        };

        let variable = after_open[..end].trim();
        if variable.is_empty() {
            return Err(Error::Template {
                template: name.to_owned(),
                reason: "empty placeholder `{{ }}`".to_owned(),
            });
        }

        let Some(value) = variables.resolve(variable) else {
            let suggestion = variables.closest(variable).map_or_else(
                || {
                    let mut known = variables.names();
                    known.sort_unstable();
                    format!("known variables: {}", known.join(", "))
                },
                |close| format!("did you mean `{close}`?"),
            );
            return Err(Error::Template {
                template: name.to_owned(),
                reason: format!("unknown variable `{variable}`; {suggestion}"),
            });
        };

        // Appended as text and never rescanned, so a value that looks like a placeholder
        // stays literal.
        out.push_str(value);
        rest = &after_open[end + CLOSE.len()..];
    }

    out.push_str(rest);
    Ok(out)
}

/// Levenshtein distance, for suggesting the variable an author probably meant.
fn edit_distance(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b_chars.len()).collect();
    let mut current = vec![0; b_chars.len() + 1];

    for (i, a_char) in a.chars().enumerate() {
        current[0] = i + 1;
        for (j, b_char) in b_chars.iter().enumerate() {
            let substitution = previous[j] + usize::from(a_char != *b_char);
            current[j + 1] = substitution.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b_chars.len()]
}

/// The default templates, embedded so a single binary needs no sibling `templates/` directory
/// (bash located it via `readlink -f "$0"`, hence its OSX shim).
///
/// The control file is not here: the Debian backend writes it, being the only place that knows
/// its grammar and can compute `Installed-Size`. Maintainer scripts are not here either: the
/// backends compose them from snippets at build time, so what ships is what was tested.
const BUILTIN: &[(&str, &str)] = &[
    ("default", include_str!("../../templates/default")),
    ("executable", include_str!("../../templates/executable")),
    (
        "systemd.service",
        include_str!("../../templates/systemd.service"),
    ),
    ("sysv-init", include_str!("../../templates/sysv-init")),
    ("upstart.conf", include_str!("../../templates/upstart.conf")),
    (
        "tmpfiles.conf",
        include_str!("../../templates/tmpfiles.conf"),
    ),
];

/// Every built-in template name, sorted.
#[must_use]
pub fn builtin_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = BUILTIN.iter().map(|(name, _)| *name).collect();
    names.sort_unstable();
    names
}

/// The source of a built-in template; the error lists the valid names.
pub fn builtin(name: &str) -> Result<&'static str> {
    BUILTIN
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, source)| *source)
        .ok_or_else(|| Error::Template {
            template: name.to_owned(),
            reason: format!(
                "unknown template; valid names: {}",
                builtin_names().join(", ")
            ),
        })
}

/// Loads a template, preferring a user-supplied override. A missing override is an error
/// naming the path, never a silent fall back to the built-in.
pub fn load(name: &str, override_path: Option<&std::path::Path>) -> Result<String> {
    match override_path {
        Some(path) => std::fs::read_to_string(path).map_err(|e| Error::io(path.to_path_buf(), e)),
        None => builtin(name).map(ToOwned::to_owned),
    }
}

#[cfg(test)]
mod tests {
    /// Without this the constant is a second, unverified copy — the bash listing was grepped
    /// out of comments that nothing forced to match the code.
    #[test]
    fn the_published_vocabulary_is_what_a_configuration_populates() {
        let manifest: crate::core::Manifest = serde_json::from_str(
            r#"{"name":"app","version":"1.2.3","description":"d","author":"A <a@example.com>",
                "nativepkg":{"init":"none"}}"#,
        )
        .expect("fixture manifest");
        let (config, _) =
            crate::core::resolve(&manifest, &crate::core::resolve::Overrides::default())
                .expect("resolves");
        let variables = Variables::for_config(&config, "0.1.0", "1.2.3", "amd64");

        let mut defined: Vec<&str> = variables.names();
        defined.sort_unstable();
        let mut published = Variables::vocabulary();
        published.sort_unstable();

        assert_eq!(
            defined, published,
            "the published vocabulary and the one a build actually uses have drifted apart"
        );
    }

    use super::*;

    fn vars() -> Variables {
        Variables::new()
            .with("package_name", "probe-app")
            .with("package_version", "1.2.3")
            .with("package_description", "a probe")
            .with("package_description_shell", "a probe")
            .with("package_maintainer", "A <a@example.com>")
            .with("package_maintainer_shell", "A <a@example.com>")
            .with("package_dependencies", "nodejs")
            .with("package_architecture", "all")
            .with("executable_name", "probe-app")
            .with("install_dir", "/usr/lib")
            // `--no-rebuild` is not plumbed through yet; `0` is bash's own default.
            .with("no_rebuild", "0")
            .with("generator_version", "0.1.0")
            .with("install_strategy", "copy")
            .with("cli_entrypoint", "app.js")
            .with("daemon_entrypoint", "app.js")
            .with("user", "probe-app")
            .with("group", "probe-app")
            .with("init", "systemd")
    }

    fn render_ok(source: &str) -> String {
        render("test", source, &vars()).expect("should render")
    }

    #[test]
    fn substitutes_a_placeholder() {
        assert_eq!(
            render_ok("Package: {{ package_name }}"),
            "Package: probe-app"
        );
    }

    #[test]
    fn tolerates_whitespace_inside_the_delimiters() {
        assert_eq!(render_ok("{{package_name}}"), "probe-app");
        assert_eq!(render_ok("{{    package_name    }}"), "probe-app");
    }

    /// The reproduction that aborts the bash renderer outright.
    #[test]
    fn a_multi_line_value_renders_literally() {
        let v = Variables::new().with("package_description", "line one\nline two");
        let out = render("test", "Description: {{ package_description }}", &v)
            .expect("a newline must not be special");
        assert_eq!(out, "Description: line one\nline two");
    }

    #[test]
    fn regex_and_shell_metacharacters_render_unchanged() {
        let hostile = r"a&b/c\d$e*f[g]h^i|j";
        let v = Variables::new().with("package_name", hostile);
        let out = render("test", "X: {{ package_name }}", &v).expect("should render");
        assert_eq!(out, format!("X: {hostile}"));
    }

    #[test]
    fn a_value_that_is_itself_a_placeholder_is_not_re_expanded() {
        let v = Variables::new()
            .with("package_name", "{{ package_version }}")
            .with("package_version", "1.2.3");
        let out = render("test", "{{ package_name }}", &v).expect("should render");
        assert_eq!(out, "{{ package_version }}");
    }

    #[test]
    fn an_unknown_variable_is_an_error_naming_template_and_variable() {
        let err = render("control", "{{ nonexistent_thing }}", &vars())
            .expect_err("an unknown variable must not be passed through");
        let message = err.to_string();
        assert!(message.contains("control"), "{message}");
        assert!(message.contains("nonexistent_thing"), "{message}");
    }

    #[test]
    fn a_near_miss_suggests_the_intended_variable() {
        let err = render("control", "{{ pakcage_name }}", &vars())
            .expect_err("a typo must not be passed through")
            .to_string();
        assert!(
            err.contains("did you mean `package_name`"),
            "a transposition should be suggested: {err}"
        );
    }

    #[test]
    fn an_unrelated_name_lists_the_vocabulary_rather_than_guessing() {
        let err = render("control", "{{ zzzzzzzzzzzzzzzz }}", &vars())
            .expect_err("unknown")
            .to_string();
        assert!(
            err.contains("known variables:"),
            "an unrelated name should not produce a misleading suggestion: {err}"
        );
    }

    #[test]
    fn supplying_an_unused_variable_is_allowed() {
        let v = vars().with("something_extra", "unused");
        assert!(render("test", "{{ package_name }}", &v).is_ok());
    }

    #[test]
    fn an_unterminated_placeholder_is_an_error() {
        let err = render("test", "Package: {{ package_name", &vars()).expect_err("unterminated");
        assert!(err.to_string().contains("unterminated"), "{err}");
    }

    /// Regression: the context was sliced at a byte offset, so a straddled multi-byte
    /// character panicked inside the error construction. A scan for `unwrap`/`panic!` had
    /// reported the module clean.
    #[test]
    fn an_unterminated_placeholder_with_multibyte_text_errors_instead_of_panicking() {
        // The emoji are positioned so a 32-byte cut lands inside one of them.
        let source = format!("{{{{{}", "\u{1F600}".repeat(9));
        let err = render("t", &source, &Variables::new())
            .expect_err("an unterminated placeholder must be an error");
        assert!(err.to_string().contains("unterminated"), "{err}");
    }

    #[test]
    fn unterminated_placeholder_context_is_truncated_by_characters() {
        let source = format!("{{{{{}", "\u{4F60}".repeat(200));
        let err = render("t", &source, &Variables::new()).expect_err("unterminated");
        let message = err.to_string();
        assert!(message.contains("unterminated"), "{message}");

        // Exact equality: `<=` cannot tell a character cap from a reverted byte cap (10 vs 30
        // characters here, both under 32). The context starts at `{{`, so the payload gets
        // `CONTEXT_CHARS - OPEN.len()`.
        let payload_chars = message.chars().filter(|c| *c == '\u{4F60}').count();
        assert_eq!(
            payload_chars,
            CONTEXT_CHARS - OPEN.len(),
            "context must be capped by characters, not bytes: {message}"
        );
    }

    #[test]
    fn multibyte_text_around_a_valid_placeholder_renders_intact() {
        let v = Variables::new().with("package_name", "\u{043F}\u{0440}\u{043E}\u{0431}\u{0430}");
        let out = render("t", "\u{4F60}\u{597D} {{ package_name }} \u{1F600}", &v)
            .expect("multi-byte text must render");
        assert_eq!(
            out,
            "\u{4F60}\u{597D} \u{043F}\u{0440}\u{043E}\u{0431}\u{0430} \u{1F600}"
        );
    }

    #[test]
    fn an_empty_placeholder_is_an_error() {
        assert!(render("test", "{{ }}", &vars()).is_err());
    }

    #[test]
    fn text_without_placeholders_passes_through() {
        let source = "#!/bin/sh\nset -e\necho hello\n";
        assert_eq!(render_ok(source), source);
    }

    #[test]
    fn every_builtin_template_renders_with_the_full_vocabulary() {
        for name in builtin_names() {
            let source = builtin(name).expect("built-in should exist");
            render(name, source, &vars())
                .unwrap_or_else(|e| panic!("built-in template `{name}` failed to render: {e}"));
        }
    }

    #[test]
    fn every_builtin_declares_its_provenance_where_comments_are_possible() {
        // Every remaining built-in supports comments and should say what produced it.
        for name in builtin_names() {
            let source = builtin(name).expect("built-in should exist");
            assert!(
                source.contains("{{ generator_version }}"),
                "built-in `{name}` should record the generator version"
            );
        }
    }

    #[test]
    fn an_unknown_builtin_lists_the_valid_names() {
        let err = builtin("nope").expect_err("unknown template").to_string();
        assert!(err.contains("default"), "{err}");
        assert!(err.contains("systemd.service"), "{err}");
    }

    #[test]
    fn a_user_override_replaces_the_builtin() {
        // Unique directory per test so a concurrent run cannot race on the path.
        let dir = std::env::temp_dir().join("nativepkg-template-override-replaces-builtin");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("control");
        std::fs::write(&path, "OVERRIDDEN {{ package_name }}").expect("write");

        let source = load("control", Some(path.as_path())).expect("override should load");
        std::fs::remove_file(&path).ok();
        assert!(source.starts_with("OVERRIDDEN"));
    }

    #[test]
    fn a_missing_override_names_the_path() {
        let err = load(
            "control",
            Some(std::path::Path::new("/nonexistent/control")),
        )
        .expect_err("a missing override must not silently fall back to the built-in");
        assert!(err.to_string().contains("/nonexistent/control"), "{err}");
    }

    #[test]
    fn edit_distance_is_symmetric_and_zero_on_equality() {
        assert_eq!(edit_distance("package_name", "package_name"), 0);
        assert_eq!(
            edit_distance("pakcage_name", "package_name"),
            edit_distance("package_name", "pakcage_name")
        );
    }
}
