//! Errors for the turbo fast-path reader.

use std::fmt;

#[derive(Debug)]
pub enum TurboError {
    Io(std::io::Error),
    Inflate(String),
    Format(String),
    MissingPart(String),
    Arrow(String),
    /// A row/column insert or delete was refused because performing it would
    /// corrupt the workbook (all-or-nothing). Carries the human-readable reason.
    Refused(String),
}

impl fmt::Display for TurboError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TurboError::Io(e) => write!(f, "io error: {e}"),
            TurboError::Inflate(e) => write!(f, "inflate error: {e}"),
            TurboError::Format(e) => write!(f, "format error: {e}"),
            TurboError::MissingPart(e) => write!(f, "missing part: {e}"),
            TurboError::Arrow(e) => write!(f, "arrow error: {e}"),
            TurboError::Refused(e) => write!(f, "refused: {e}"),
        }
    }
}

impl std::error::Error for TurboError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TurboError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for TurboError {
    fn from(e: std::io::Error) -> Self {
        TurboError::Io(e)
    }
}

pub type TurboResult<T> = Result<T, TurboError>;
