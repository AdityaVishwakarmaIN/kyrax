use crate::types::idx_or_name::IdxOrName;
use calamine::XlsxError;
use std::{error::Error, fmt::Display};

/// The kind of a kyrax error.
#[derive(Debug)]
pub enum KyraxErrorKind {
    UnsupportedColumnTypeCombination(String),
    CannotRetrieveCellData(usize, usize),
    CalamineCellError(calamine::CellErrorType),
    CalamineError(calamine::Error),
    SheetNotFound(IdxOrName),
    ColumnNotFound(IdxOrName),
    // Arrow errors can be of several different types (arrow::error::Error, PyError), and having
    // the actual type has not much value for us, so we just store a string context
    ArrowError(String),
    InvalidParameters(String),
    InvalidColumn(String),
    Internal(String),
}

impl Display for KyraxErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KyraxErrorKind::UnsupportedColumnTypeCombination(detail) => {
                write!(f, "unsupported column type combination: {detail}")
            }
            KyraxErrorKind::CannotRetrieveCellData(row, col) => {
                write!(f, "cannot retrieve cell data at ({row}, {col})")
            }
            KyraxErrorKind::CalamineCellError(calamine_error) => {
                write!(f, "calamine cell error: {calamine_error}")
            }
            KyraxErrorKind::CalamineError(calamine_error) => {
                write!(f, "calamine error: {calamine_error}")
            }
            KyraxErrorKind::SheetNotFound(idx_or_name) => {
                let message = idx_or_name.format_message();
                write!(f, "sheet {message} not found")
            }
            KyraxErrorKind::ColumnNotFound(idx_or_name) => {
                let message = idx_or_name.format_message();
                write!(f, "column {message} not found")
            }
            KyraxErrorKind::ArrowError(err) => write!(f, "arrow error: {err}"),
            KyraxErrorKind::InvalidParameters(err) => write!(f, "invalid parameters: {err}"),
            KyraxErrorKind::InvalidColumn(err) => write!(f, "invalid column: {err}"),
            KyraxErrorKind::Internal(err) => write!(f, "kyrax error: {err}"),
        }
    }
}

/// A `kyrax` error.
///
/// Contains a kind and a context. Use the `Display` trait to format the
/// error message with its context.
#[derive(Debug)]
pub struct KyraxError {
    pub kind: KyraxErrorKind,
    pub context: Vec<String>,
}

pub(crate) trait ErrorContext {
    fn with_context<S: ToString, F>(self, ctx_fn: F) -> Self
    where
        F: FnOnce() -> S;
}

impl KyraxError {
    pub(crate) fn new(kind: KyraxErrorKind) -> Self {
        Self {
            kind,
            context: vec![],
        }
    }
}

impl Display for KyraxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{kind}", kind = self.kind)?;
        if !self.context.is_empty() {
            writeln!(f, "\nContext:")?;

            self.context
                .iter()
                .enumerate()
                .try_for_each(|(idx, ctx_value)| writeln!(f, "    {idx}: {ctx_value}"))?;
        }
        Ok(())
    }
}

impl Error for KyraxError {}

impl ErrorContext for KyraxError {
    fn with_context<S: ToString, F>(mut self, ctx_fn: F) -> Self
    where
        F: FnOnce() -> S,
    {
        self.context.push(ctx_fn().to_string());
        self
    }
}

impl From<KyraxErrorKind> for KyraxError {
    fn from(kind: KyraxErrorKind) -> Self {
        KyraxError::new(kind)
    }
}

impl From<XlsxError> for KyraxError {
    fn from(err: XlsxError) -> Self {
        KyraxErrorKind::CalamineError(calamine::Error::Xlsx(err)).into()
    }
}

pub type KyraxResult<T> = Result<T, KyraxError>;

impl<T> ErrorContext for KyraxResult<T> {
    fn with_context<S: ToString, F>(self, ctx_fn: F) -> Self
    where
        F: FnOnce() -> S,
    {
        match self {
            Ok(_) => self,
            Err(e) => Err(e.with_context(ctx_fn)),
        }
    }
}

/// Contains Python versions of our custom errors
#[cfg(feature = "python")]
pub(crate) mod py_errors {
    use super::KyraxErrorKind;
    use crate::error;
    use pyo3::{PyErr, PyResult, create_exception, exceptions::PyException};

    // Base kyrax error
    create_exception!(
        _kyrax,
        KyraxError,
        PyException,
        "The base class for all kyrax errors"
    );
    // Unsupported column type
    create_exception!(
        _kyrax,
        UnsupportedColumnTypeCombinationError,
        KyraxError,
        "Column contains an unsupported type combination"
    );
    // Cannot retrieve cell data
    create_exception!(
        _kyrax,
        CannotRetrieveCellDataError,
        KyraxError,
        "Data for a given cell cannot be retrieved"
    );
    // Calamine cell error
    create_exception!(
        _kyrax,
        CalamineCellError,
        KyraxError,
        "calamine returned an error regarding the content of the cell"
    );
    // Calamine error
    create_exception!(
        _kyrax,
        CalamineError,
        KyraxError,
        "Generic calamine error"
    );
    // Sheet not found
    create_exception!(
        _kyrax,
        SheetNotFoundError,
        KyraxError,
        "Sheet was not found"
    );
    // Sheet not found
    create_exception!(
        _kyrax,
        ColumnNotFoundError,
        KyraxError,
        "Column was not found"
    );
    // Arrow error
    create_exception!(
        _kyrax,
        ArrowError,
        KyraxError,
        "Generic arrow error"
    );
    // Invalid parameters
    create_exception!(
        _kyrax,
        InvalidParametersError,
        KyraxError,
        "Provided parameters are invalid"
    );
    // Invalid column
    create_exception!(
        _kyrax,
        InvalidColumnError,
        KyraxError,
        "Column is invalid"
    );
    // Internal error
    create_exception!(
        _kyrax,
        InternalError,
        KyraxError,
        "Internal kyrax error"
    );

    impl From<error::KyraxError> for PyErr {
        fn from(err: error::KyraxError) -> Self {
            let message = err.to_string();
            match err.kind {
                KyraxErrorKind::UnsupportedColumnTypeCombination(_) => {
                    UnsupportedColumnTypeCombinationError::new_err(message)
                }
                KyraxErrorKind::CannotRetrieveCellData(_, _) => {
                    CannotRetrieveCellDataError::new_err(message)
                }
                KyraxErrorKind::CalamineCellError(_) => CalamineCellError::new_err(message),
                KyraxErrorKind::CalamineError(_) => CalamineError::new_err(message),
                KyraxErrorKind::SheetNotFound(_) => SheetNotFoundError::new_err(message),
                KyraxErrorKind::ColumnNotFound(_) => ColumnNotFoundError::new_err(message),
                KyraxErrorKind::ArrowError(_) => ArrowError::new_err(message),
                KyraxErrorKind::InvalidParameters(_) => {
                    InvalidParametersError::new_err(message)
                }
                KyraxErrorKind::InvalidColumn(_) => InvalidColumnError::new_err(message),
                KyraxErrorKind::Internal(_) => ArrowError::new_err(message),
            }
        }
    }

    pub(crate) trait IntoPyResult {
        type Inner;

        fn into_pyresult(self) -> PyResult<Self::Inner>;
    }

    impl<T> IntoPyResult for super::KyraxResult<T> {
        type Inner = T;

        fn into_pyresult(self) -> PyResult<Self::Inner> {
            self.map_err(Into::into)
        }
    }
}
