//! Typed errors shared by every stage of the build pipeline.

use std::path::PathBuf;

pub type Result<T> = core::result::Result<T, Error>;

/// Coarse on purpose: callers distinguish bad input from a bad environment, not individual
/// messages. `#[non_exhaustive]` so variants can be added without a breaking release.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("i/o error at `{path}`")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Keeps the parser error as `source`, so the CLI can render the chain and the position.
    #[error("failed to parse `{path}` as JSON")]
    ManifestParse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    /// Valid JSON that does not describe a buildable package. Carries no path: produced by
    /// resolution, which does no I/O; the CLI attaches the path where it is known.
    #[error("manifest error: {message}")]
    Manifest { message: String },

    #[error("invalid package name `{name}`: {reason}")]
    InvalidPackageName { name: String, reason: String },

    #[error("invalid {kind} name `{name}`: {reason}")]
    InvalidUnixName {
        /// `user` or `group`.
        kind: &'static str,
        name: String,
        reason: String,
    },

    #[error("invalid version `{version}`: {reason}")]
    InvalidVersion { version: String, reason: String },

    #[error("template error in `{template}`: {reason}")]
    Template { template: String, reason: String },
}

impl Error {
    pub fn manifest(message: impl Into<String>) -> Self {
        Self::Manifest {
            message: message.into(),
        }
    }

    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_implements_std_error() {
        fn assert_std_error<T: std::error::Error>() {}
        assert_std_error::<Error>();
    }

    #[test]
    fn every_variant_renders_a_non_empty_message() {
        let cases = [
            Error::io("/tmp/x", std::io::Error::other("boom")),
            Error::manifest("no `name` field"),
            Error::InvalidPackageName {
                name: "@a/b".into(),
                reason: "contains `/`".into(),
            },
            Error::InvalidVersion {
                version: String::new(),
                reason: "empty".into(),
            },
            Error::Template {
                template: "control".into(),
                reason: "unknown variable".into(),
            },
        ];
        for case in cases {
            assert!(!case.to_string().is_empty(), "empty Display for {case:?}");
        }
    }

    #[test]
    fn io_variant_exposes_source() {
        use std::error::Error as _;
        let err = Error::io("/tmp/x", std::io::Error::other("boom"));
        assert!(
            err.source().is_some(),
            "Io must expose the underlying error as source"
        );
    }
}
