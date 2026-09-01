//! Errors specific to writing an RPM package. Kept out of [`nativepkg_core::Error`] so format
//! knowledge stays out of the format-agnostic crate.

use std::path::PathBuf;

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error(transparent)]
    Core(#[from] nativepkg_core::Error),

    #[error("rpm error: {context}")]
    Rpm {
        context: String,
        #[source]
        source: rpm::Error,
    },

    #[error("i/o error at `{path}`")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A plan entry with no RPM equivalent.
    #[error("cannot package `{destination}`: {reason}")]
    Unrepresentable { destination: String, reason: String },
}

impl Error {
    pub fn rpm(context: impl Into<String>, source: rpm::Error) -> Self {
        Self::Rpm {
            context: context.into(),
            source,
        }
    }

    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
