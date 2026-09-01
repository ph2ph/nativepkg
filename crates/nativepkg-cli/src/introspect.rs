//! Answering "what does this tool support?" from the tool's own data.
//!
//! The bash implementation grepped its own `# HELPDOC:` comments, which nothing kept in step
//! with the code. Everything here reads the structures the build path reads, so they cannot
//! drift.

use anyhow::{Context as _, Result};
use nativepkg_core::npm::SETTING_KEYS;
use nativepkg_core::template::{self, Variables};

/// The manifest keys that override configuration.
#[must_use]
pub fn json_overrides() -> Vec<&'static str> {
    // A test binds SETTING_KEYS to the manifest struct, so a new setting is listed as soon as
    // it exists.
    SETTING_KEYS.to_vec()
}

#[must_use]
pub fn template_variables() -> Vec<&'static str> {
    Variables::vocabulary()
}

#[must_use]
pub fn templates() -> Vec<&'static str> {
    template::builtin_names()
}

/// The source of one built-in template; an unknown name fails listing the ones that exist.
pub fn cat_template(name: &str) -> Result<&'static str> {
    template::builtin(name).with_context(|| {
        format!(
            "no template named `{name}`; available: {}",
            templates().join(", ")
        )
    })
}

/// Embedded, so `--show-readme` works away from the source tree (the bash tool paged a file
/// from next to the script).
#[must_use]
pub fn readme() -> &'static str {
    include_str!("../../../README.md")
}

#[must_use]
pub fn changelog() -> &'static str {
    include_str!("../../../CHANGELOG.md")
}
