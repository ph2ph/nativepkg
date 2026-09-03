//! Checks on free text before it reaches a line-oriented file or a shell: a line break in
//! `author` becomes a new control field, a `"` in the description ends a shell string.
//! Everything a manifest can supply is checked at resolution time, so templates and backends
//! can trust it.

use crate::core::error::{Error, Result};

/// Rejects control characters other than newline and tab: for multi-line text.
pub fn printable(field: &str, value: &str) -> Result<()> {
    match value
        .chars()
        .find(|c| c.is_control() && !matches!(c, '\n' | '\t'))
    {
        Some(c) => Err(reject(field, c)),
        None => Ok(()),
    }
}

/// Rejects every control character: for fields that must stay on one line.
pub fn single_line(field: &str, value: &str) -> Result<()> {
    match value.chars().find(char::is_ascii_control) {
        Some(c) => Err(reject(field, c)),
        None => printable(field, value),
    }
}

/// [`single_line`] plus no whitespace at all: for a URL.
pub fn token(field: &str, value: &str) -> Result<()> {
    single_line(field, value)?;
    match value.chars().find(|c| c.is_whitespace()) {
        Some(c) => Err(reject(field, c)),
        None => Ok(()),
    }
}

/// `value` as the inside of a double-quoted shell string.
#[must_use]
pub fn shell_double_quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 8);
    for c in value.chars() {
        if matches!(c, '\\' | '"' | '$' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn reject(field: &str, c: char) -> Error {
    let what = match c {
        '\n' | '\r' => "a line break".to_owned(),
        c if c.is_whitespace() => "whitespace".to_owned(),
        c => format!("the control character U+{:04X}", u32::from(c)),
    };
    Error::manifest(format!(
        "`{field}` contains {what}; it is written into package metadata, which cannot carry it"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_breaks_are_refused_where_one_line_is_expected() {
        assert!(single_line("author", "A <a@x.y>\nPre-Depends: evil").is_err());
        assert!(single_line("author", "A <a@x.y>").is_ok());
        assert!(printable("description", "one\ntwo\tthree").is_ok());
        assert!(printable("description", "one\r\ntwo").is_err());
        assert!(printable("description", "nul\0").is_err());
        assert!(token("homepage", "https://x.y/z").is_ok());
        assert!(token("homepage", "https://x.y/ z").is_err());
    }

    #[test]
    fn shell_quoting_escapes_exactly_the_four_characters_that_matter_in_double_quotes() {
        assert_eq!(
            shell_double_quoted(r#"a "b" $c `d` e\f"#),
            r#"a \"b\" \$c \`d\` e\\f"#
        );
        assert_eq!(
            shell_double_quoted("plain 'single' text"),
            "plain 'single' text"
        );
    }
}
