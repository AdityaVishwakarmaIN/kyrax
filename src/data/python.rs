use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;
use std::{fmt::Debug, ops::Not};

use arrow_array::{
    Array, ArrayRef, BooleanArray, Date32Array, DurationMillisecondArray, Float64Array, Int64Array,
    NullArray, RecordBatch, StringArray, TimestampMillisecondArray,
};
use arrow_schema::{Field, Schema};
use calamine::{CellType, DataType, Range};

use super::cell_extractors;
use crate::{
    data::{ExcelSheetData, RowSelector, generate_row_selector},
    error::{ErrorContext, KyraxErrorKind, KyraxResult},
    types::{
        dtype::{DType, DTypeCoercion, cell_dtype, resolve_coerced_dtype},
        excelsheet::{
            CellError, CellErrors, SkipRows,
            column_info::{ColumnInfo, DTypeFrom},
        },
    },
};

/// A column whose dtype had to be widened at materialization time because the sampled dtype could
/// not hold every value in the column.
#[derive(Debug, Clone)]
pub(crate) struct DTypePromotion {
    pub(crate) column: String,
    pub(crate) from: DType,
    pub(crate) to: DType,
}

thread_local! {
    /// Promotions recorded while building the current `RecordBatch`.
    ///
    /// A thread-local is used deliberately: the arrow conversion runs inside `Python::detach`
    /// (i.e. with the GIL *released*), so it cannot touch Python objects to raise a warning.
    /// `detach` does not move work to another thread, so the python binding can drain this after
    /// the detached section returns, with the GIL held again.
    static DTYPE_PROMOTIONS: RefCell<Vec<DTypePromotion>> = const { RefCell::new(Vec::new()) };
}

fn clear_dtype_promotions() {
    DTYPE_PROMOTIONS.with(|p| p.borrow_mut().clear());
}

fn record_dtype_promotion(column: &str, from: DType, to: DType) {
    DTYPE_PROMOTIONS.with(|p| {
        p.borrow_mut().push(DTypePromotion {
            column: column.to_owned(),
            from,
            to,
        })
    });
}

/// Drains the dtype promotions recorded by the most recent `RecordBatch` build on this thread.
pub(crate) fn take_dtype_promotions() -> Vec<DTypePromotion> {
    DTYPE_PROMOTIONS.with(|p| std::mem::take(&mut *p.borrow_mut()))
}

/// Emits one `UserWarning` per column that had its dtype widened at materialization time.
///
/// Must be called with the GIL held, i.e. *after* the `Python::detach` section that built the
/// `RecordBatch`.
pub(crate) fn warn_dtype_promotions(py: pyo3::Python<'_>, source: &str) -> pyo3::PyResult<()> {
    use pyo3::exceptions::PyUserWarning;

    for promotion in take_dtype_promotions() {
        pyo3::PyErr::warn(
            py,
            &py.get_type::<PyUserWarning>(),
            std::ffi::CString::new(format!(
                "column \"{column}\" of {source} was inferred as {from} from a sample of the \
                 column, but holds values that do not fit it; the column was read as {to} instead. \
                 Pass an explicit dtype, or raise schema_sample_rows, to silence this.",
                column = promotion.column,
                from = promotion.from,
                to = promotion.to,
            ))
            .map_err(|err| {
                pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "could not build dtype promotion warning: {err}"
                ))
            })?
            .as_c_str(),
            1,
        )?;
    }

    Ok(())
}

mod with_error_impls {
    use super::*;

    pub(crate) fn create_boolean_array_with_errors<CT: CellType + DataType + Debug>(
        data: &Range<CT>,
        col: usize,
        offset: usize,
        limit: usize,
    ) -> (Arc<dyn Array>, Vec<CellError>) {
        let mut cell_errors = vec![];

        let arr = Arc::new(BooleanArray::from_iter((offset..limit).map(|row| {
            data.get((row, col)).and_then(|cell| {
                if cell.is_empty() {
                    None
                } else if let Some(b) = cell_extractors::extract_boolean(cell) {
                    Some(b)
                } else {
                    cell_errors.push(CellError {
                        position: (row, col),
                        row_offset: offset,
                        detail: format!("Expected boolean but got '{cell:?}"),
                    });
                    None
                }
            })
        })));

        (arr, cell_errors)
    }

    pub(crate) fn create_int_array_with_errors<CT: CellType + DataType + Debug>(
        data: &Range<CT>,
        col: usize,
        offset: usize,
        limit: usize,
    ) -> (Arc<dyn Array>, Vec<CellError>) {
        let mut cell_errors = vec![];

        let arr = Arc::new(Int64Array::from_iter((offset..limit).map(|row| {
            data.get((row, col)).and_then(|cell| {
                if cell.is_empty() {
                    None
                } else {
                    match cell_extractors::extract_int(cell) {
                        Some(value) => Some(value),
                        None => {
                            cell_errors.push(CellError {
                                position: (row, col),
                                row_offset: offset,
                                detail: format!("Expected int but got '{cell:?}'"),
                            });
                            None
                        }
                    }
                }
            })
        })));
        (arr, cell_errors)
    }

    pub(crate) fn create_float_array_with_errors<CT: CellType + DataType + Debug>(
        data: &Range<CT>,
        col: usize,
        offset: usize,
        limit: usize,
    ) -> (Arc<dyn Array>, Vec<CellError>) {
        let mut cell_errors = vec![];

        let arr = Arc::new(Float64Array::from_iter((offset..limit).map(|row| {
            data.get((row, col)).and_then(|cell| {
                if cell.is_empty() {
                    None
                } else {
                    match cell_extractors::extract_float(cell) {
                        Some(value) => Some(value),
                        None => {
                            cell_errors.push(CellError {
                                position: (row, col),
                                row_offset: offset,
                                detail: format!("Expected float but got '{cell:?}'"),
                            });
                            None
                        }
                    }
                }
            })
        })));
        (arr, cell_errors)
    }

    pub(crate) fn create_string_array_with_errors<CT: CellType + DataType + Debug>(
        data: &Range<CT>,
        col: usize,
        offset: usize,
        limit: usize,
        whitespace_as_null: bool,
    ) -> (Arc<dyn Array>, Vec<CellError>) {
        let mut cell_errors = vec![];

        let arr = Arc::new(StringArray::from_iter((offset..limit).map(|row| {
            data.get((row, col)).and_then(|cell| {
                if cell.is_empty() {
                    None
                } else {
                    match cell_extractors::extract_string(cell) {
                        Some(value) => {
                            if whitespace_as_null && value.trim().is_empty() {
                                None
                            } else {
                                Some(value)
                            }
                        }
                        None => {
                            cell_errors.push(CellError {
                                position: (row, col),
                                row_offset: offset,
                                detail: format!("Expected string but got '{cell:?}'"),
                            });
                            None
                        }
                    }
                }
            })
        })));

        (arr, cell_errors)
    }

    pub(crate) fn create_date_array_with_errors<CT: CellType + DataType + Debug>(
        data: &Range<CT>,
        col: usize,
        offset: usize,
        limit: usize,
    ) -> (Arc<dyn Array>, Vec<CellError>) {
        let mut cell_errors = vec![];

        let arr = Arc::new(Date32Array::from_iter((offset..limit).map(|row| {
            data.get((row, col)).and_then(|cell| {
                if cell.is_empty() {
                    None
                } else {
                    match cell_extractors::extract_date_as_num_days(cell) {
                        Some(value) => Some(value),
                        None => {
                            cell_errors.push(CellError {
                                position: (row, col),
                                row_offset: offset,
                                detail: format!("Expected date but got '{:?}'", cell),
                            });
                            None
                        }
                    }
                }
            })
        })));

        (arr, cell_errors)
    }

    pub(crate) fn create_datetime_array_with_errors<CT: CellType + DataType + Debug>(
        data: &Range<CT>,
        col: usize,
        offset: usize,
        limit: usize,
    ) -> (Arc<dyn Array>, Vec<CellError>) {
        let mut cell_errors = vec![];
        let arr = Arc::new(TimestampMillisecondArray::from_iter((offset..limit).map(
            |row| {
                data.get((row, col)).and_then(|cell| {
                    if cell.is_empty() {
                        None
                    } else {
                        match cell_extractors::extract_datetime_as_timestamp_ms(cell) {
                            Some(value) => Some(value),
                            None => {
                                cell_errors.push(CellError {
                                    position: (row, col),
                                    row_offset: offset,
                                    detail: format!("Expected datetime but got '{:?}'", cell),
                                });
                                None
                            }
                        }
                    }
                })
            },
        )));
        (arr, cell_errors)
    }

    pub(crate) fn create_duration_array_with_errors<CT: CellType + DataType + Debug>(
        data: &Range<CT>,
        col: usize,
        offset: usize,
        limit: usize,
    ) -> (Arc<dyn Array>, Vec<CellError>) {
        let mut cell_errors = vec![];
        let arr = Arc::new(DurationMillisecondArray::from_iter((offset..limit).map(
            |row| {
                data.get((row, col)).and_then(|cell| {
                    if cell.is_empty() {
                        None
                    } else {
                        match cell_extractors::extract_duration_as_ms(cell) {
                            Some(value) => Some(value),
                            None => {
                                cell_errors.push(CellError {
                                    position: (row, col),
                                    row_offset: offset,
                                    detail: format!("Expected duration but got '{cell:?}'"),
                                });
                                None
                            }
                        }
                    }
                })
            },
        )));
        (arr, cell_errors)
    }
}

// NOTE: the unchecked per-dtype builders (int/float/bool/date/datetime/duration) were removed in
// favour of `build_array_checked`, which builds the same arrays but also reports cells that had to
// be nulled because they did not fit the sampled dtype. `create_string_array` survives because
// string is the top of the coercion lattice and can never be lossy.
pub(crate) fn create_string_array<CT: CellType + DataType>(
    data: &Range<CT>,
    col: usize,
    row_iter: impl Iterator<Item = usize>,
    whitespace_as_null: bool,
) -> Arc<dyn Array> {
    Arc::new(if whitespace_as_null {
        StringArray::from_iter(row_iter.map(|row| {
            data.get((row, col))
                .and_then(cell_extractors::extract_string)
                // Only return the string if it contains non-whitespace characters
                .filter(|s| s.trim().is_empty().not())
        }))
    } else {
        StringArray::from_iter(row_iter.map(|row| {
            data.get((row, col))
                .and_then(cell_extractors::extract_string)
        }))
    })
}

macro_rules! create_array_function_with_errors {
    ($func_name:ident) => {
        pub(crate) fn $func_name(
            data: &ExcelSheetData,
            col: usize,
            offset: usize,
            limit: usize,
        ) -> (Arc<dyn Array>, Vec<CellError>) {
            match data {
                ExcelSheetData::Owned(range) => {
                    with_error_impls::$func_name(range, col, offset, limit)
                }
                ExcelSheetData::Ref(range) => {
                    with_error_impls::$func_name(range, col, offset, limit)
                }
            }
        }
    };
}

create_array_function_with_errors!(create_boolean_array_with_errors);
create_array_function_with_errors!(create_int_array_with_errors);
create_array_function_with_errors!(create_float_array_with_errors);
create_array_function_with_errors!(create_date_array_with_errors);
create_array_function_with_errors!(create_datetime_array_with_errors);
create_array_function_with_errors!(create_duration_array_with_errors);

pub(crate) fn create_string_array_with_errors(
    data: &ExcelSheetData,
    col: usize,
    offset: usize,
    limit: usize,
    whitespace_as_null: bool,
) -> (Arc<dyn Array>, Vec<CellError>) {
    match data {
        ExcelSheetData::Owned(range) => with_error_impls::create_string_array_with_errors(
            range,
            col,
            offset,
            limit,
            whitespace_as_null,
        ),
        ExcelSheetData::Ref(range) => with_error_impls::create_string_array_with_errors(
            range,
            col,
            offset,
            limit,
            whitespace_as_null,
        ),
    }
}

/// Whether a cell legitimately materializes as null, whatever the column's dtype.
///
/// Empty cells, the recognized NULL sentinel strings, whitespace-only strings (when
/// `whitespace_as_null` is set) and the "nullable" error cells all map to null by design. Anything
/// else that fails to extract is *data loss*.
fn cell_is_nullish<CT: CellType + DataType + Debug>(cell: &CT, whitespace_as_null: bool) -> bool {
    // Fast path: an empty cell is by far the most common reason an extraction yields nothing, and
    // classifying it fully would walk every `is_*` branch in `cell_dtype`.
    if cell.is_empty() {
        return true;
    }
    // An unclassifiable cell is conservatively treated as nullish: promoting on it would not
    // recover anything anyway, and `promoted_dtype` ignores it too.
    cell_dtype(cell, whitespace_as_null).map_or(true, |dtype| dtype == DType::Null)
}

/// Builds the arrow array for `dtype`, reporting whether any cell holding a real value had to be
/// nulled because it did not fit `dtype`.
///
/// `DType::String` is the top of the coercion lattice, so it is never reported as lossy: there is
/// nothing wider to promote it to.
fn build_array_checked<CT: CellType + DataType + Debug>(
    dtype: DType,
    data: &Range<CT>,
    col: usize,
    row_selector: &RowSelector,
    row_count: usize,
    whitespace_as_null: bool,
    detect_lossy: bool,
) -> (ArrayRef, bool) {
    macro_rules! checked {
        ($ArrTy:ty, $extract:path) => {{
            let mut lossy = false;
            let arr = Arc::new(<$ArrTy>::from_iter(row_selector.iter().map(|row| {
                let Some(cell) = data.get((row, col)) else {
                    return None;
                };
                let value = $extract(cell);
                if detect_lossy && value.is_none() && !cell_is_nullish(cell, whitespace_as_null) {
                    lossy = true;
                }
                value
            }))) as ArrayRef;
            (arr, lossy)
        }};
    }

    match dtype {
        // An all-null column in the sample may still hold real values further down.
        DType::Null => (
            Arc::new(NullArray::new(row_count)) as ArrayRef,
            detect_lossy && promoted_dtype(data, col, row_selector, whitespace_as_null).is_some(),
        ),
        DType::Int => checked!(Int64Array, cell_extractors::extract_int),
        DType::Float => checked!(Float64Array, cell_extractors::extract_float),
        DType::Bool => checked!(BooleanArray, cell_extractors::extract_boolean),
        DType::Date => checked!(Date32Array, cell_extractors::extract_date_as_num_days),
        DType::DateTime => checked!(
            TimestampMillisecondArray,
            cell_extractors::extract_datetime_as_timestamp_ms
        ),
        DType::Duration => checked!(
            DurationMillisecondArray,
            cell_extractors::extract_duration_as_ms
        ),
        DType::String => (
            create_string_array(data, col, row_selector.iter(), whitespace_as_null),
            false,
        ),
    }
}

/// Re-derives the dtype of a column from *every* selected row (as opposed to the sampled prefix
/// used at schema-inference time).
///
/// Returns `None` when the column holds no non-null value at all, so that a genuinely empty column
/// keeps its `DType::Null` rather than being widened to string.
fn promoted_dtype<CT: CellType + DataType + Debug>(
    data: &Range<CT>,
    col: usize,
    row_selector: &RowSelector,
    whitespace_as_null: bool,
) -> Option<DType> {
    let mut column_types: HashSet<DType> = row_selector
        .iter()
        .filter_map(|row| data.get((row, col)))
        .filter_map(|cell| cell_dtype(cell, whitespace_as_null).ok())
        .collect();

    column_types.remove(&DType::Null);

    if column_types.is_empty() {
        return None;
    }

    // If the combination has no lossless common type, fall back to string: a silent schema change
    // is far better than silently dropping the values.
    Some(resolve_coerced_dtype(&column_types).unwrap_or(DType::String))
}

/// Converts a list of ColumnInfo to an arrow Schema
pub(crate) fn selected_columns_to_schema(columns: &[ColumnInfo]) -> Schema {
    let fields: Vec<_> = columns.iter().map(Into::<Field>::into).collect();
    Schema::new(fields)
}

/// Creates an arrow RecordBatch from an Iterator over (column_name, column data tuples) and an arrow schema
pub(crate) fn record_batch_from_name_array_iterator<
    'a,
    I: Iterator<Item = (&'a str, Arc<dyn Array>)>,
>(
    iter: I,
    schema: Schema,
) -> KyraxResult<RecordBatch> {
    let mut iter = iter.peekable();
    // If the iterable is empty, try_from_iter returns an Err
    if iter.peek().is_none() {
        Ok(RecordBatch::new_empty(Arc::new(schema)))
    } else {
        // We use `try_from_iter_with_nullable` because `try_from_iter` relies on `array.null_count() > 0;`
        // to determine if the array is nullable. This is not the case for `NullArray` which has no nulls.
        RecordBatch::try_from_iter_with_nullable(iter.map(|(field_name, array)| {
            let nullable = array.is_nullable();
            (field_name, array, nullable)
        }))
        .map_err(|err| KyraxErrorKind::ArrowError(err.to_string()).into())
        .with_context(|| "could not create RecordBatch from iterable")
    }
}

/// Creates an arrow `RecordBatch` from `ExcelSheetData`. Expects the following parameters:
/// * `columns`: a slice of `ColumnInfo`, representing the columns that should be extracted from the range
/// * `data`: the sheets data, as an `ExcelSheetData`
/// * `offset`: the row index at which to start
/// * `limit`: the row index at which to stop (excluded)
pub(crate) fn record_batch_from_data_and_columns<CT: CellType + DataType + Debug>(
    columns: &[ColumnInfo],
    data: &Range<CT>,
    offset: usize,
    limit: usize,
    whitespace_as_null: bool,
    dtype_coercion: &DTypeCoercion,
) -> KyraxResult<RecordBatch> {
    // Use RowSelector::Range for simple offset..limit case - no Vec allocation!
    let row_selector = RowSelector::Range(offset..limit);
    record_batch_from_data_and_columns_with_row_selector(
        columns,
        data,
        &row_selector,
        whitespace_as_null,
        dtype_coercion,
    )
}

pub(crate) fn record_batch_from_data_and_columns_with_skip_rows<CT: CellType + DataType + Debug>(
    columns: &[ColumnInfo],
    data: &Range<CT>,
    skip_rows: &SkipRows,
    offset: usize,
    limit: usize,
    whitespace_as_null: bool,
    dtype_coercion: &DTypeCoercion,
) -> KyraxResult<RecordBatch> {
    // Generate row selector - ranges for simple cases, filtered Vec only when needed
    let row_selector = generate_row_selector(skip_rows, offset, limit)?;
    record_batch_from_data_and_columns_with_row_selector(
        columns,
        data,
        &row_selector,
        whitespace_as_null,
        dtype_coercion,
    )
}

fn record_batch_from_data_and_columns_with_row_selector<CT: CellType + DataType + Debug>(
    columns: &[ColumnInfo],
    data: &Range<CT>,
    row_selector: &RowSelector,
    whitespace_as_null: bool,
    dtype_coercion: &DTypeCoercion,
) -> KyraxResult<RecordBatch> {
    // NOTE: `schema` is only used for the zero-column case; `record_batch_from_name_array_iterator`
    // otherwise derives the batch schema from the arrays themselves, which is what allows a
    // promoted column to carry its widened type all the way out.
    let schema = selected_columns_to_schema(columns);
    let row_count = row_selector.len();

    clear_dtype_promotions();

    let mut arrays: Vec<(&str, ArrayRef)> = Vec::with_capacity(columns.len());

    for column_info in columns {
        let col_idx = column_info.index;
        let dtype = column_info.dtype;

        // Guessed dtypes are inferred from a sample of the column (`schema_sample_rows`, 1000 by
        // default), so a value further down may not fit the dtype we picked. Building the array
        // reports that case instead of silently nulling those cells.
        //
        // A dtype the *user* asked for is never second-guessed: coercing (and thus nulling) values
        // that do not fit is the documented behaviour of an explicit `dtypes=` argument.
        let promotable = matches!(column_info.dtype_from, DTypeFrom::Guessed);

        let (array, lossy) = build_array_checked(
            dtype,
            data,
            col_idx,
            row_selector,
            row_count,
            whitespace_as_null,
            promotable,
        );

        let array = match lossy
            .then(|| promoted_dtype(data, col_idx, row_selector, whitespace_as_null))
            .flatten()
            .filter(|promoted| *promoted != dtype)
        {
            Some(promoted) => {
                if matches!(dtype_coercion, DTypeCoercion::Strict) {
                    return Err(KyraxErrorKind::UnsupportedColumnTypeCombination(format!(
                        "type coercion is strict and column \"{name}\" contains values that do not \
                         fit its inferred dtype {dtype} (holding them requires {promoted}). The \
                         dtype was inferred from a sample of the column: consider raising \
                         schema_sample_rows or specifying the dtype explicitly",
                        name = column_info.name
                    ))
                    .into());
                }
                record_dtype_promotion(&column_info.name, dtype, promoted);
                build_array_checked(
                    promoted,
                    data,
                    col_idx,
                    row_selector,
                    row_count,
                    whitespace_as_null,
                    // The promoted dtype is the widest type the column actually needs; there is
                    // nothing further to promote to, so skip the detection work on the rebuild.
                    false,
                )
                .0
            }
            None => array,
        };

        arrays.push((column_info.name.as_str(), array));
    }

    record_batch_from_name_array_iterator(arrays.into_iter(), schema)
}

pub(crate) fn record_batch_from_data_and_columns_with_errors(
    columns: &[ColumnInfo],
    data: &ExcelSheetData,
    offset: usize,
    limit: usize,
    whitespace_as_null: bool,
    dtype_coercion: &DTypeCoercion,
) -> KyraxResult<(RecordBatch, CellErrors)> {
    let schema = selected_columns_to_schema(columns);
    let mut cell_errors = vec![];
    let mut arrays: Vec<(&str, ArrayRef)> = Vec::with_capacity(columns.len());

    let row_selector = RowSelector::Range(offset..limit);

    for column_info in columns {
        let col_idx = column_info.index;
        let inferred_dtype = column_info.dtype;

        let dtype = match data {
            ExcelSheetData::Ref(range) => {
                promoted_dtype(range, col_idx, &row_selector, whitespace_as_null)
                    .unwrap_or(inferred_dtype)
            }
            _ => inferred_dtype,
        };

        if dtype != inferred_dtype && matches!(dtype_coercion, DTypeCoercion::Strict) {
            return Err(KyraxErrorKind::UnsupportedColumnTypeCombination(format!(
                "type coercion is strict and column \"{name}\" contains values that do not \
                 fit its inferred dtype {inferred_dtype} (holding them requires {dtype}).",
                name = column_info.name
            ))
            .into());
        }

        let (array, new_cell_errors) = match dtype {
            DType::Null => (Arc::new(NullArray::new(limit - offset)) as ArrayRef, vec![]),
            DType::Int => create_int_array_with_errors(data, col_idx, offset, limit),
            DType::Float => create_float_array_with_errors(data, col_idx, offset, limit),
            DType::String => {
                create_string_array_with_errors(data, col_idx, offset, limit, whitespace_as_null)
            }
            DType::Bool => create_boolean_array_with_errors(data, col_idx, offset, limit),
            DType::DateTime => create_datetime_array_with_errors(data, col_idx, offset, limit),
            DType::Date => create_date_array_with_errors(data, col_idx, offset, limit),
            DType::Duration => create_duration_array_with_errors(data, col_idx, offset, limit),
        };

        cell_errors.extend(new_cell_errors);
        arrays.push((column_info.name.as_str(), array));
    }

    let batch = record_batch_from_name_array_iterator(arrays.into_iter(), schema)?;
    Ok((
        batch,
        CellErrors {
            errors: cell_errors,
        },
    ))
}

impl RowSelector {
    pub(crate) fn iter(&self) -> Box<dyn Iterator<Item = usize> + '_> {
        match self {
            RowSelector::Range(range) => Box::new(range.clone()),
            RowSelector::Filtered(vec) => Box::new(vec.iter().copied()),
        }
    }
}
