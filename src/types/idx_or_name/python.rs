use pyo3::{
    Borrowed, Bound, FromPyObject, IntoPyObject, IntoPyObjectExt, PyAny, PyErr, Python,
    types::PyAnyMethods,
};

use crate::{
    error::{KyraxError, KyraxErrorKind, KyraxResult, py_errors::IntoPyResult},
    types::idx_or_name::IdxOrName,
};

impl TryFrom<&Bound<'_, PyAny>> for IdxOrName {
    type Error = KyraxError;

    fn try_from(value: &Bound<'_, PyAny>) -> KyraxResult<Self> {
        if let Ok(index) = value.extract() {
            Ok(Self::Idx(index))
        } else if let Ok(name) = value.extract() {
            Ok(Self::Name(name))
        } else {
            Err(KyraxErrorKind::InvalidParameters(format!(
                "cannot create IdxOrName from {value:?}"
            ))
            .into())
        }
    }
}

impl<'a, 'py> FromPyObject<'a, 'py> for IdxOrName {
    type Error = PyErr;
    fn extract(ob: Borrowed<'a, 'py, PyAny>) -> Result<Self, Self::Error> {
        (&*ob).try_into().into_pyresult()
    }
}

impl<'py> IntoPyObject<'py> for IdxOrName {
    type Target = PyAny;

    type Output = Bound<'py, Self::Target>;

    type Error = pyo3::PyErr;

    fn into_pyobject(self, py: Python<'py>) -> Result<Self::Output, Self::Error> {
        match self {
            IdxOrName::Idx(idx) => idx.into_bound_py_any(py),
            IdxOrName::Name(name) => name.into_bound_py_any(py),
        }
    }
}

impl<'py> IntoPyObject<'py> for &IdxOrName {
    type Target = PyAny;

    type Output = Bound<'py, Self::Target>;

    type Error = pyo3::PyErr;

    fn into_pyobject(self, py: Python<'py>) -> Result<Self::Output, Self::Error> {
        match self {
            IdxOrName::Idx(idx) => idx.into_bound_py_any(py),
            IdxOrName::Name(name) => name.into_bound_py_any(py),
        }
    }
}
