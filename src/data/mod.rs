mod cell_extractors;
#[cfg(feature = "python")]
mod python;
mod rust;
use chrono::{Duration, NaiveDate, NaiveDateTime};
#[cfg(feature = "python")]
pub(crate) use python::*;

use calamine::{CellType, Data as CalData, DataRef as CalDataRef, DataType, Range};

use crate::{
    data::rust::{
        create_boolean_vec, create_date_vec, create_datetime_vec, create_duration_vec,
        create_float_vec, create_int_vec, create_string_vec,
    },
    error::{KyraxErrorKind, KyraxResult},
    types::{
        dtype::{DType, DTypeCoercion, get_dtype_for_column},
        excelsheet::{SkipRows, column_info::ColumnInfo},
    },
};

#[derive(Debug)]
pub(crate) enum ExcelSheetData<'r> {
    Owned(Range<CalData>),
    Ref(Range<CalDataRef<'r>>),
}

impl ExcelSheetData<'_> {
    pub(crate) fn width(&self) -> usize {
        match self {
            ExcelSheetData::Owned(range) => range.width(),
            ExcelSheetData::Ref(range) => range.width(),
        }
    }

    pub(crate) fn height(&self) -> usize {
        match self {
            ExcelSheetData::Owned(range) => range.height(),
            ExcelSheetData::Ref(range) => range.height(),
        }
    }

    pub(super) fn get_as_string(&self, pos: (usize, usize)) -> Option<String> {
        match self {
            ExcelSheetData::Owned(range) => range.get(pos).and_then(|data| data.as_string()),
            ExcelSheetData::Ref(range) => range.get(pos).and_then(|data| data.as_string()),
        }
    }

    pub(crate) fn dtype_for_column(
        &self,
        start_row: usize,
        end_row: usize,
        col: usize,
        dtype_coercion: &DTypeCoercion,
        whitespace_as_null: bool,
    ) -> KyraxResult<DType> {
        match self {
            ExcelSheetData::Owned(data) => get_dtype_for_column(
                data,
                start_row,
                end_row,
                col,
                dtype_coercion,
                whitespace_as_null,
            ),
            ExcelSheetData::Ref(data) => get_dtype_for_column(
                data,
                start_row,
                end_row,
                col,
                dtype_coercion,
                whitespace_as_null,
            ),
        }
    }

    pub(crate) fn height_without_tail_whitespace(&self) -> usize {
        match self {
            ExcelSheetData::Owned(data) => {
                height_without_tail_whitespace(data).unwrap_or_else(|| data.height())
            }
            ExcelSheetData::Ref(data) => {
                height_without_tail_whitespace(data).unwrap_or_else(|| data.height())
            }
        }
    }

    pub(crate) fn start(&self) -> Option<(usize, usize)> {
        let start = match self {
            ExcelSheetData::Owned(range) => range.start(),
            ExcelSheetData::Ref(range) => range.start(),
        };
        start.map(|(r, c)| (r as usize, c as usize))
    }
}

impl From<Range<CalData>> for ExcelSheetData<'_> {
    fn from(range: Range<CalData>) -> Self {
        Self::Owned(range)
    }
}

impl<'a> From<Range<CalDataRef<'a>>> for ExcelSheetData<'a> {
    fn from(range: Range<CalDataRef<'a>>) -> Self {
        Self::Ref(range)
    }
}

trait CellIsWhiteSpace {
    fn is_whitespace(&self) -> bool;
}

impl<T> CellIsWhiteSpace for T
where
    T: DataType,
{
    fn is_whitespace(&self) -> bool {
        if self.is_empty() {
            true
        } else if self.is_string()
            && let Some(s) = self.get_string()
        {
            s.trim().is_empty()
        } else {
            false
        }
    }
}

pub(crate) fn height_without_tail_whitespace<CT: CellType + DataType + std::fmt::Debug>(
    data: &Range<CT>,
) -> Option<usize> {
    let height = data.height();
    let width = data.width();
    if height < 1 {
        return Some(0);
    }
    if width < 1 {
        return None;
    }
    (0..width)
        .map(|col_idx| {
            let mut row_idx = height - 1;
            // Start at the bottom of the column and work upwards until we find a non-empty cell
            while row_idx > 0
                && data
                    .get((row_idx, col_idx))
                    .map(CellIsWhiteSpace::is_whitespace)
                    .unwrap_or(true)
            {
                row_idx -= 1;
            }
            row_idx + 1
        })
        .max()
}

/// A container for a typed vector of values. Used to represent a column of data in an Excel sheet.
/// These should only be used when you need to work on the raw data. Otherwise, you should use a
/// `KyraxColumn`.
#[derive(Debug, Clone, PartialEq)]
pub enum KyraxSeries {
    Null,
    Bool(Vec<Option<bool>>),
    String(Vec<Option<String>>),
    Int(Vec<Option<i64>>),
    Float(Vec<Option<f64>>),
    Datetime(Vec<Option<NaiveDateTime>>),
    Date(Vec<Option<NaiveDate>>),
    Duration(Vec<Option<Duration>>),
}

impl KyraxSeries {
    pub fn dtype(&self) -> DType {
        match self {
            KyraxSeries::Null => DType::Null,
            KyraxSeries::Bool(_) => DType::Bool,
            KyraxSeries::String(_) => DType::String,
            KyraxSeries::Int(_) => DType::Int,
            KyraxSeries::Float(_) => DType::Float,
            KyraxSeries::Datetime(_) => DType::DateTime,
            KyraxSeries::Date(_) => DType::Date,
            KyraxSeries::Duration(_) => DType::Duration,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, KyraxSeries::Null)
    }
}

macro_rules! impl_series_variant {
    ($type:ty, $variant:ident, $into_fn:ident) => {
        impl From<Vec<Option<$type>>> for KyraxSeries {
            fn from(vec: Vec<Option<$type>>) -> Self {
                Self::$variant(vec)
            }
        }

        impl<const N: usize> From<[Option<$type>; N]> for KyraxSeries {
            fn from(arr: [Option<$type>; N]) -> Self {
                Self::$variant(arr.to_vec())
            }
        }

        impl<const N: usize> From<[$type; N]> for KyraxSeries {
            fn from(arr: [$type; N]) -> Self {
                Self::$variant(arr.into_iter().map(Some).collect())
            }
        }

        impl From<&[$type]> for KyraxSeries {
            fn from(arr: &[$type]) -> Self {
                Self::$variant(arr.into_iter().map(|it| Some(it.to_owned())).collect())
            }
        }

        impl From<&[Option<$type>]> for KyraxSeries {
            fn from(arr: &[Option<$type>]) -> Self {
                Self::$variant(arr.into_iter().map(ToOwned::to_owned).collect())
            }
        }

        // Not implementing is_empty here, because we have no len information for null Series
        impl KyraxSeries {
            pub fn $into_fn(self) -> KyraxResult<Vec<Option<$type>>> {
                if let Self::$variant(vec) = self {
                    Ok(vec)
                } else {
                    Err(KyraxErrorKind::InvalidParameters(format!(
                        "{self:?} cannot be converted to {type_name}",
                        type_name = std::any::type_name::<$type>()
                    ))
                    .into())
                }
            }
        }
    };
}

impl_series_variant!(bool, Bool, into_bools);
impl_series_variant!(String, String, into_strings);
impl_series_variant!(i64, Int, into_ints);
impl_series_variant!(f64, Float, into_floats);
impl_series_variant!(NaiveDateTime, Datetime, into_datetimes);
impl_series_variant!(NaiveDate, Date, into_dates);
impl_series_variant!(Duration, Duration, into_durations);

// Conflicting impls when using `From<AsRef<[&str]>>`
impl<const N: usize> From<[Option<&str>; N]> for KyraxSeries {
    fn from(arr: [Option<&str>; N]) -> Self {
        Self::String(arr.into_iter().map(|s| s.map(|s| s.to_string())).collect())
    }
}

impl<const N: usize> From<[&str; N]> for KyraxSeries {
    fn from(arr: [&str; N]) -> Self {
        Self::String(arr.into_iter().map(|s| Some(s.to_string())).collect())
    }
}

/// A column in a sheet or table. A wrapper around a `KyraxSeries` and a name.
#[derive(Debug, Clone, PartialEq)]
pub struct KyraxColumn {
    pub name: String,
    pub(crate) data: KyraxSeries,
    len: usize,
}

impl KyraxColumn {
    pub fn try_new(name: String, data: KyraxSeries, len: Option<usize>) -> KyraxResult<Self> {
        let data_len = match &data {
            KyraxSeries::Null => None,
            KyraxSeries::Bool(v) => Some(v.len()),
            KyraxSeries::String(v) => Some(v.len()),
            KyraxSeries::Int(v) => Some(v.len()),
            KyraxSeries::Float(v) => Some(v.len()),
            KyraxSeries::Datetime(v) => Some(v.len()),
            KyraxSeries::Date(v) => Some(v.len()),
            KyraxSeries::Duration(v) => Some(v.len()),
        };
        if let Some(len) = len
            && let Some(data_len) = data_len
            && data_len != len
        {
            return Err(KyraxErrorKind::InvalidColumn(format!(
                "Column '{name}' has length {data_len} but expected {len}"
            ))
            .into());
        }
        let len = len.or(data_len).ok_or_else(|| {
            KyraxErrorKind::InvalidColumn("`len` is mandatory for `KyraxSeries::Null`".to_string())
        })?;
        Ok(Self { name, data, len })
    }

    /// Create a new null series with the given name and length.
    pub fn new_null<S: Into<String>>(name: S, len: usize) -> Self {
        Self {
            name: name.into(),
            data: KyraxSeries::Null,
            len,
        }
    }

    pub(crate) fn try_from_column_info<CT: CellType + DataType>(
        column_info: &ColumnInfo,
        data: &Range<CT>,
        offset: usize,
        limit: usize,
        whitespace_as_null: bool,
    ) -> KyraxResult<Self> {
        let len = limit.checked_sub(offset).ok_or_else(|| {
            KyraxErrorKind::InvalidParameters(format!(
                "limit is smaller than offset: {limit} is smaller than {offset}"
            ))
        })?;
        let data = match column_info.dtype {
            DType::Null => KyraxSeries::Null,
            DType::Int => KyraxSeries::Int(create_int_vec(data, column_info.index, offset, limit)),
            DType::Float => {
                KyraxSeries::Float(create_float_vec(data, column_info.index, offset, limit))
            }
            DType::String => KyraxSeries::String(create_string_vec(
                data,
                column_info.index,
                offset,
                limit,
                whitespace_as_null,
            )),
            DType::Bool => {
                KyraxSeries::Bool(create_boolean_vec(data, column_info.index, offset, limit))
            }
            DType::DateTime => {
                KyraxSeries::Datetime(create_datetime_vec(data, column_info.index, offset, limit))
            }
            DType::Date => {
                KyraxSeries::Date(create_date_vec(data, column_info.index, offset, limit))
            }
            DType::Duration => {
                KyraxSeries::Duration(create_duration_vec(data, column_info.index, offset, limit))
            }
        };
        Ok(Self {
            name: column_info.name.clone(),
            data,
            len,
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn data(&self) -> &KyraxSeries {
        &self.data
    }
}

impl From<KyraxColumn> for KyraxSeries {
    fn from(column: KyraxColumn) -> Self {
        column.data
    }
}

/// Enum for lazy row selection - avoids materializing Vec for simple cases
#[derive(Debug)]
pub(crate) enum RowSelector {
    /// Simple range - no Vec allocation needed
    Range(std::ops::Range<usize>),
    /// Pre-filtered list of specific row indices
    Filtered(Vec<usize>),
}

impl RowSelector {
    pub(crate) fn len(&self) -> usize {
        match self {
            RowSelector::Range(range) => range.len(),
            RowSelector::Filtered(vec) => vec.len(),
        }
    }
}

/// Generate row selector based on [`SkipRows`] and range limits
pub(crate) fn generate_row_selector(
    skip_rows: &SkipRows,
    offset: usize,
    limit: usize,
) -> KyraxResult<RowSelector> {
    match skip_rows {
        SkipRows::Simple(_skip_count) => {
            // For simple case, the offset has already been adjusted by pagination logic
            // So we just return the normal range - no Vec allocation!
            Ok(RowSelector::Range(offset..limit))
        }
        SkipRows::SkipEmptyRowsAtBeginning => {
            // For empty rows at beginning, calamine handles this at the header level
            // So we just return the normal range - no Vec allocation!
            Ok(RowSelector::Range(offset..limit))
        }
        SkipRows::List(skip_set) => {
            // Filter out rows that are in the skip set
            // `skip_set` contains data-relative indices, but we need to work with absolute indices
            let filtered: Vec<usize> = (offset..limit)
                .enumerate()
                .filter_map(|(data_row_idx, absolute_row_idx)| {
                    (!skip_set.contains(&data_row_idx)).then_some(absolute_row_idx)
                })
                .collect();
            Ok(RowSelector::Filtered(filtered))
        }
        #[cfg(feature = "python")]
        SkipRows::Callable(_func) => {
            // Call the Python function for each row to determine if it should be skipped
            // The callable should receive data-relative row indices (0, 1, 2, ...)
            pyo3::Python::attach(|py| {
                Ok(RowSelector::Filtered(
                    (offset..limit)
                        .enumerate()
                        .filter_map(|(data_row_idx, absolute_row_idx)| {
                            (!skip_rows.should_skip_row(data_row_idx, py).unwrap_or(false))
                                .then_some(absolute_row_idx)
                        })
                        .collect(),
                ))
            })
        }
    }
}
