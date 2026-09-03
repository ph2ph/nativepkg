//! Errors specific to writing an Arch Linux package.

use std::path::PathBuf;

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error(transparent)]
    Core(#[from] crate::core::Error),

    #[error("i/o error at `{path}`")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("`{path}` was {planned} bytes when planned but {actual} bytes when read")]
    SourceChanged {
        path: PathBuf,
        planned: u64,
        actual: u64,
    },

    #[error("could not write the archive: {reason}")]
    Archive {
        reason: String,
        #[source]
        source: Option<std::io::Error>,
    },
}

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    pub fn archive(reason: impl Into<String>, source: std::io::Error) -> Self {
        Self::Archive {
            reason: reason.into(),
            source: Some(source),
        }
    }
}
