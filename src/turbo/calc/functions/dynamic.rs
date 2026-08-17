// functions/dynamic.rs — the dynamic-array function family. Owned exclusively
// by the dynamic-array family agent; no other agent edits this file.
//
// Registry contract: implement `register` below and keep this exact signature.
// Do NOT edit functions/mod.rs — the `mod dynamic;` declaration and the
// `dynamic::register(&mut r)` call site in `build()` are already final.
// See functions/mod.rs for the worked ABS template.
//
// These functions return ARRAYS (a 1x1 array is a legitimate result that the
// evaluator treats like a scalar), so every registration here sets
// `array_aware: true` — the eval loop must hand them arrays as-is, never
// scalarized. RANDARRAY is additionally `volatile: true`: its result must
// never be cached as a `<v>`, or the workbook would show stale random numbers.
//
// Correctness policy (shared with the other families): an empty result is an
// Excel error (#CALC! unless the function has its own documented error), never
// a zero-size array; SORT/SORTBY are stable; TAKE/DROP/CHOOSECOLS/CHOOSEROWS
// honour negative counts as "from the end"; HSTACK/VSTACK pad ragged input
// with #N/A rather than erroring; TEXTSPLIT has separate column and row
// delimiters plus ignore_empty and pad_with.
//
// NOTE ON XLOOKUP: this file implements the complete dynamic-array XLOOKUP
// (every match_mode and search_mode, block returns for wide return arrays) but
// deliberately does NOT register it. The lookup family already registers a
// scalar XLOOKUP (functions/lookup.rs), and `Registry::register` debug_asserts
// on a duplicate name, which would panic the whole debug test build. The
// coordinator must remove lookup.rs's XLOOKUP and add the one line below.
use super::{FuncArg, FuncCtx, FuncSpec, Registry};
use crate::turbo::calc::coerce::{
    coerce_number, coerce_text, compare, compare_eq, number_to_general,
};
use crate::turbo::calc::value::{ArrayValue, CalcError, CalcValue};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

/// Cell cap for shapes a dynamic function is willing to allocate (~4M cells ≈
/// 64 MB of `CalcValue`, the same budget the evaluator uses). Larger results
/// are `#VALUE!`, never a huge allocation.
const CELL_CAP: u64 = 4_000_000;

/// `|n|` guarded against non-finite results (#NUM! instead of a NaN value).
fn ok_num(n: f64) -> Result<CalcValue, CalcError> {
    if n.is_finite() {
        Ok(CalcValue::Number(n))
    } else {
        Err(CalcError::Num)
    }
}

/// Wrap any value as a dense array; scalars become 1x1.
fn to_array(v: CalcValue) -> ArrayValue {
    match v {
        CalcValue::Array(a) => (*a).clone(),
        other => ArrayValue::new(1, 1, vec![other]),
    }
}

/// Reject a result shape above `CELL_CAP`.
fn check_cap(rows: u32, cols: u32) -> Result<(), CalcError> {
    if rows as u64 * cols as u64 > CELL_CAP {
        Err(CalcError::Value)
    } else {
        Ok(())
    }
}

/// Excel's TRUE/FALSE argument coercion (SORT `by_col`, UNIQUE flags, ...):
/// booleans, numbers (non-zero = TRUE), the literal strings "TRUE"/"FALSE".
/// Blank defaults to FALSE — the documented default for the dynamic flags.
fn as_bool_arg(v: &CalcValue) -> Result<bool, CalcError> {
    match v {
        CalcValue::Bool(b) => Ok(*b),
        CalcValue::Number(n) => Ok(*n != 0.0),
        CalcValue::Blank => Ok(false),
        CalcValue::Text(t) => match t.trim().to_ascii_uppercase().as_str() {
            "TRUE" => Ok(true),
            "FALSE" => Ok(false),
            _ => Ok(coerce_number(v)? != 0.0),
        },
        CalcValue::Error(e) => Err(*e),
        CalcValue::Array(_) => Err(CalcError::Value),
    }
}

/// Whether one FILTER `include` cell keeps its row/column. TRUE and non-zero
/// keep; FALSE, 0 and blank drop; errors propagate.
fn include_keep(v: &CalcValue) -> Result<bool, CalcError> {
    match v {
        CalcValue::Error(e) => Err(*e),
        CalcValue::Bool(b) => Ok(*b),
        CalcValue::Number(n) => Ok(*n != 0.0),
        CalcValue::Blank => Ok(false),
        CalcValue::Text(_) => Ok(coerce_number(v)? != 0.0),
        CalcValue::Array(_) => Err(CalcError::Value),
    }
}

/// Comparison for SORT/SORTBY keys. Errors sort after every value (and
/// equal to each other), which is how Excel orders error cells; ordinary
/// values use the global Excel ordering. Never propagates.
fn sort_cmp(l: &CalcValue, r: &CalcValue) -> Ordering {
    match (l, r) {
        (CalcValue::Error(_), CalcValue::Error(_)) => Ordering::Equal,
        (CalcValue::Error(_), _) => Ordering::Greater,
        (_, CalcValue::Error(_)) => Ordering::Less,
        _ => compare(l, r).unwrap_or(Ordering::Equal),
    }
}

/// A canonical, type-tagged string key for one value, so UNIQUE can hash
/// rows/columns. Text is lower-cased to match the engine's case-insensitive
/// comparison semantics.
fn canonical_key(v: &CalcValue) -> String {
    match v {
        CalcValue::Number(n) => format!("n{}", number_to_general(*n)),
        CalcValue::Text(t) => format!("t{}", t.to_ascii_lowercase()),
        CalcValue::Bool(b) => format!("b{}", b),
        CalcValue::Blank => "z".to_string(),
        CalcValue::Error(e) => format!("e{}", e.code()),
        CalcValue::Array(a) => format!("a{}x{}", a.rows, a.cols),
    }
}

/// The UNIQUE key of one row (when `by_col` is false) or one column (when
/// true). Elements are joined with a unit separator so `["a","b"]` never
/// collides with `["ab"]`.
fn slice_key(a: &ArrayValue, idx: u32, by_col: bool) -> String {
    let mut s = String::new();
    if by_col {
        for r in 0..a.rows {
            s.push_str(&canonical_key(a.get(r, idx)));
            s.push('\u{1f}');
        }
    } else {
        for c in 0..a.cols {
            s.push_str(&canonical_key(a.get(idx, c)));
            s.push('\u{1f}');
        }
    }
    s
}

// -- position scans shared by XLOOKUP and XMATCH -----------------------------

/// First (or last, when `keep_last`) exact-match position; with `wildcards`
/// the needle is a `*`/`?`/`~` pattern. Errors propagate.
fn exact_scan(
    values: &[CalcValue],
    needle: &CalcValue,
    wildcards: bool,
    keep_last: bool,
) -> Result<Option<usize>, CalcError> {
    let mut found = None;
    for (i, v) in values.iter().enumerate() {
        if compare_eq(v, needle, wildcards)? {
            found = Some(i);
            if !keep_last {
                break;
            }
        }
    }
    Ok(found)
}

/// Approximate scan over the global Excel ordering: with `larger` false the
/// largest value <= needle, with `larger` true the smallest value >= needle.
/// Ties keep the first position unless `keep_last` (the last-to-first search
/// direction).
fn best_scan(
    values: &[CalcValue],
    needle: &CalcValue,
    larger: bool,
    keep_last: bool,
) -> Result<Option<usize>, CalcError> {
    let mut best: Option<(CalcValue, usize)> = None;
    for (i, v) in values.iter().enumerate() {
        let ord = compare(v, needle)?;
        let in_band = if larger {
            ord != Ordering::Less
        } else {
            ord != Ordering::Greater
        };
        if !in_band {
            continue;
        }
        match &best {
            None => best = Some((v.clone(), i)),
            Some((bv, _)) => {
                let o = compare(v, bv)?;
                let better = if larger {
                    o == Ordering::Less
                } else {
                    o == Ordering::Greater
                };
                if better || (o == Ordering::Equal && keep_last) {
                    best = Some((v.clone(), i));
                }
            }
        }
    }
    Ok(best.map(|(_, i)| i))
}

/// Binary search over a (documented-as-sorted) array, honouring the match
/// mode: 0 exact, -1 the largest value <= needle, 1 the smallest value >=
/// needle. `ascending` selects the data's order for search_mode 2 vs -2; in
/// descending data the direction of "next" flips because larger values sit at
/// smaller indices. Excel does not validate that the data actually is sorted.
fn binary_scan(
    values: &[CalcValue],
    needle: &CalcValue,
    match_mode: i64,
    ascending: bool,
) -> Result<Option<usize>, CalcError> {
    let (mut lo, mut hi) = (0usize, values.len());
    let mut best = None;
    while lo < hi {
        let mid = (lo + hi) / 2;
        let ord = compare(&values[mid], needle)?;
        match match_mode {
            0 => match ord {
                Ordering::Equal => return Ok(Some(mid)),
                _ => {
                    let go_right = if ascending {
                        ord == Ordering::Less
                    } else {
                        ord == Ordering::Greater
                    };
                    if go_right {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
            },
            -1 => {
                // Eligible values are <= needle; take the rightmost (ascending)
                // or leftmost (descending) of those.
                if matches!(ord, Ordering::Less | Ordering::Equal) {
                    best = Some(mid);
                    if ascending {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                } else if ascending {
                    hi = mid;
                } else {
                    lo = mid + 1;
                }
            }
            1 => {
                // Eligible values are >= needle; take the leftmost (ascending)
                // or rightmost (descending) of those.
                if matches!(ord, Ordering::Greater | Ordering::Equal) {
                    best = Some(mid);
                    if ascending {
                        hi = mid;
                    } else {
                        lo = mid + 1;
                    }
                } else if ascending {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            _ => unreachable!(),
        }
    }
    Ok(best)
}

/// The one function the whole engine's lookup family converges on: find
/// `needle` in `values` per match_mode/search_mode, exactly as XLOOKUP and
/// XMATCH both specify.
fn lookup_position(
    values: &[CalcValue],
    needle: &CalcValue,
    match_mode: i64,
    search_mode: i64,
) -> Result<Option<usize>, CalcError> {
    match search_mode {
        1 | -1 => {
            let keep_last = search_mode == -1;
            match match_mode {
                0 => exact_scan(values, needle, false, keep_last),
                2 => exact_scan(values, needle, true, keep_last),
                -1 => best_scan(values, needle, false, keep_last),
                1 => best_scan(values, needle, true, keep_last),
                _ => unreachable!(),
            }
        }
        2 | -2 => match match_mode {
            // Wildcards and binary search do not mix; scan linearly.
            2 => exact_scan(values, needle, true, false),
            m => binary_scan(values, needle, m, search_mode == 2),
        },
        _ => unreachable!(),
    }
}

// -- XLOOKUP -----------------------------------------------------------------

/// XLOOKUP(lookup, lookup_array, return_array, [if_not_found], [match_mode],
/// [search_mode]).
///
/// match_mode: 0 exact (default), -1 exact-or-next-smaller, 1
/// exact-or-next-larger, 2 wildcard. search_mode: 1 first-to-last (default),
/// -1 last-to-first, 2 binary ascending, -2 binary descending. A missing
/// `if_not_found` yields #N/A. A return array wider (or taller) than the
/// lookup vector returns the matched row (or column) as an array — that block
/// return is what makes XLOOKUP a dynamic-array function.
///
/// Deliberately unregistered until the coordinator removes lookup.rs's scalar
/// XLOOKUP (see the module doc and `register`); dead-code-flagged here because
/// the name is not wired yet.
#[allow(dead_code)]
fn xlookup(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let needle = args[0].value(ctx)?;
    let look = to_array(args[1].value(ctx)?);
    let ret = to_array(args[2].value(ctx)?);

    let match_mode = match args.get(4) {
        Some(a) => {
            let v = a.value(ctx)?;
            if v.is_blank() {
                0
            } else {
                coerce_number(&v)? as i64
            }
        }
        None => 0,
    };
    let search_mode = match args.get(5) {
        Some(a) => {
            let v = a.value(ctx)?;
            if v.is_blank() {
                1
            } else {
                coerce_number(&v)? as i64
            }
        }
        None => 1,
    };
    if !matches!(search_mode, 1 | -1 | 2 | -2) {
        return Err(CalcError::Value);
    }
    if !matches!(match_mode, -1..=2) {
        return Err(CalcError::Value);
    }

    // The lookup array must be a single row or a single column; the return
    // array must carry the same number of elements along that dimension.
    let column_lookup = if look.cols == 1 {
        true
    } else if look.rows == 1 {
        false
    } else {
        return Err(CalcError::Value);
    };
    let n = if column_lookup { look.rows } else { look.cols };
    if column_lookup {
        if ret.rows != n {
            return Err(CalcError::Value);
        }
    } else if ret.cols != n {
        return Err(CalcError::Value);
    }

    let values: Vec<CalcValue> = if column_lookup {
        (0..look.rows).map(|r| look.get(r, 0).clone()).collect()
    } else {
        (0..look.cols).map(|c| look.get(0, c).clone()).collect()
    };

    let pos = lookup_position(&values, &needle, match_mode, search_mode)?;
    let not_found = |ctx: &FuncCtx| -> Result<CalcValue, CalcError> {
        match args.get(3) {
            Some(a) => a.value(ctx),
            None => Err(CalcError::Na),
        }
    };

    match pos {
        Some(i) => {
            let i = i as u32;
            if column_lookup {
                if ret.cols == 1 {
                    Ok(ret.get(i, 0).clone())
                } else {
                    let row: Vec<CalcValue> =
                        (0..ret.cols).map(|c| ret.get(i, c).clone()).collect();
                    Ok(CalcValue::array(ArrayValue::new(1, ret.cols, row)))
                }
            } else if ret.rows == 1 {
                Ok(ret.get(0, i).clone())
            } else {
                let col: Vec<CalcValue> = (0..ret.rows).map(|r| ret.get(r, i).clone()).collect();
                Ok(CalcValue::array(ArrayValue::new(ret.rows, 1, col)))
            }
        }
        None => not_found(ctx),
    }
}

// -- FILTER ------------------------------------------------------------------

/// FILTER(array, include, [if_empty]). `include` may be a vertical array (one
/// boolean per row) or a horizontal array (one per column); its shape must
/// match the array along that dimension. No matching rows/columns is #CALC!
/// unless `if_empty` is supplied.
fn filter(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let array = to_array(args[0].value(ctx)?);
    let include = to_array(args[1].value(ctx)?);
    let if_empty = |ctx: &FuncCtx| -> Result<CalcValue, CalcError> {
        match args.get(2) {
            Some(a) => a.value(ctx),
            None => Err(CalcError::Calc),
        }
    };

    if include.cols == 1 && include.rows == array.rows {
        let mut keep: Vec<u32> = Vec::new();
        for r in 0..array.rows {
            if include_keep(include.get(r, 0))? {
                keep.push(r);
            }
        }
        if keep.is_empty() {
            return if_empty(ctx);
        }
        check_cap(keep.len() as u32, array.cols)?;
        let mut data = Vec::with_capacity(keep.len() * array.cols as usize);
        for &r in &keep {
            for c in 0..array.cols {
                data.push(array.get(r, c).clone());
            }
        }
        return Ok(CalcValue::array(ArrayValue::new(
            keep.len() as u32,
            array.cols,
            data,
        )));
    }
    if include.rows == 1 && include.cols == array.cols {
        let mut keep: Vec<u32> = Vec::new();
        for c in 0..array.cols {
            if include_keep(include.get(0, c))? {
                keep.push(c);
            }
        }
        if keep.is_empty() {
            return if_empty(ctx);
        }
        check_cap(array.rows, keep.len() as u32)?;
        let mut data = Vec::with_capacity(array.rows as usize * keep.len());
        for r in 0..array.rows {
            for &c in &keep {
                data.push(array.get(r, c).clone());
            }
        }
        return Ok(CalcValue::array(ArrayValue::new(
            array.rows,
            keep.len() as u32,
            data,
        )));
    }
    Err(CalcError::Value)
}

// -- UNIQUE ------------------------------------------------------------------

/// UNIQUE(array, [by_col], [exactly_once]). Distinct rows (or columns when
/// `by_col`) in first-seen order; `exactly_once` keeps only keys that appear
/// exactly once.
fn unique(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let array = to_array(args[0].value(ctx)?);
    let by_col = if args.len() > 1 {
        as_bool_arg(&args[1].value(ctx)?)?
    } else {
        false
    };
    let exactly_once = if args.len() > 2 {
        as_bool_arg(&args[2].value(ctx)?)?
    } else {
        false
    };

    let n = if by_col { array.cols } else { array.rows };
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut order: Vec<u32> = Vec::new();
    for i in 0..n {
        let k = slice_key(&array, i, by_col);
        let e = seen.entry(k).or_insert(0);
        *e += 1;
        if *e == 1 {
            order.push(i);
        }
    }
    let keep: Vec<u32> = if exactly_once {
        order
            .into_iter()
            .filter(|&i| seen.get(&slice_key(&array, i, by_col)) == Some(&1))
            .collect()
    } else {
        order
    };

    let (rows, cols) = if by_col {
        (array.rows, keep.len() as u32)
    } else {
        (keep.len() as u32, array.cols)
    };
    check_cap(rows, cols)?;
    let mut data = Vec::with_capacity((rows as usize).saturating_mul(cols as usize));
    for r in 0..rows {
        for c in 0..cols {
            if by_col {
                data.push(array.get(r, keep[c as usize]).clone());
            } else {
                data.push(array.get(keep[r as usize], c).clone());
            }
        }
    }
    Ok(CalcValue::array(ArrayValue::new(rows, cols, data)))
}

// -- SORT --------------------------------------------------------------------

/// SORT(array, [sort_index], [sort_order], [by_col]). One stable sort of an
/// index vector, never repeated passes. sort_order is 1 ascending / -1
/// descending; by_col defaults to FALSE (sort by row).
fn sort(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let array = to_array(args[0].value(ctx)?);
    let sort_index = if args.len() > 1 {
        coerce_number(&args[1].value(ctx)?)?.trunc() as i64
    } else {
        1
    };
    let sort_order = if args.len() > 2 {
        coerce_number(&args[2].value(ctx)?)? as i64
    } else {
        1
    };
    let by_col = if args.len() > 3 {
        as_bool_arg(&args[3].value(ctx)?)?
    } else {
        false
    };
    if !matches!(sort_order, 1 | -1) {
        return Err(CalcError::Value);
    }
    if by_col {
        if sort_index < 1 || sort_index > array.rows as i64 {
            return Err(CalcError::Value);
        }
    } else if sort_index < 1 || sort_index > array.cols as i64 {
        return Err(CalcError::Value);
    }

    let n = if by_col { array.cols } else { array.rows };
    let key = sort_index as u32 - 1;
    let mut idx: Vec<u32> = (0..n).collect();
    idx.sort_by(|&a, &b| {
        let (ka, kb) = if by_col {
            (array.get(key, a), array.get(key, b))
        } else {
            (array.get(a, key), array.get(b, key))
        };
        let o = sort_cmp(ka, kb);
        if sort_order == -1 { o.reverse() } else { o }
    });

    let (rows, cols) = if by_col {
        (array.rows, n)
    } else {
        (n, array.cols)
    };
    let mut data = Vec::with_capacity((rows as usize).saturating_mul(cols as usize));
    for r in 0..rows {
        for c in 0..cols {
            if by_col {
                data.push(array.get(r, idx[c as usize]).clone());
            } else {
                data.push(array.get(idx[r as usize], c).clone());
            }
        }
    }
    Ok(CalcValue::array(ArrayValue::new(rows, cols, data)))
}

// -- SORTBY ------------------------------------------------------------------

/// One key vector plus its direction for SORTBY.
fn sortby_keys(
    ctx: &FuncCtx,
    args: &[FuncArg],
    array: &ArrayValue,
) -> Result<Vec<(Vec<CalcValue>, bool)>, CalcError> {
    // Rows are the sortable dimension when the array is taller than one row;
    // a one-row array sorts its columns. Each by_array must be a matching
    // single column (resp. single row) or it is #VALUE!.
    let by_rows = array.rows > 1;
    let mut keys: Vec<(Vec<CalcValue>, bool)> = Vec::new();
    let mut pending: Option<Vec<CalcValue>> = None;
    let mut i = 1usize;
    while i < args.len() {
        let v = args[i].value(ctx)?;
        i += 1;
        match pending.take() {
            None => pending = Some(extract_key(&to_array(v), by_rows, array)?),
            Some(k) => {
                // This argument is either the order for `k` or the next by_array.
                match order_value(&v) {
                    Some(asc) => keys.push((k, asc)),
                    None => {
                        keys.push((k, true));
                        pending = Some(extract_key(&to_array(v), by_rows, array)?);
                    }
                }
            }
        }
    }
    if let Some(k) = pending {
        keys.push((k, true));
    }
    if keys.is_empty() {
        return Err(CalcError::Value);
    }
    Ok(keys)
}

/// A by_array reduced to its key vector, shape-validated against `array`.
fn extract_key(
    by: &ArrayValue,
    by_rows: bool,
    array: &ArrayValue,
) -> Result<Vec<CalcValue>, CalcError> {
    if by_rows {
        if by.cols != 1 || by.rows != array.rows {
            return Err(CalcError::Value);
        }
        Ok((0..by.rows).map(|r| by.get(r, 0).clone()).collect())
    } else {
        if by.rows != 1 || by.cols != array.cols {
            return Err(CalcError::Value);
        }
        Ok((0..by.cols).map(|c| by.get(0, c).clone()).collect())
    }
}

/// SORTBY's optional sort_order: blank defaults to ascending; only the scalar
/// 1 / -1 (or their numeric text) are an order, anything else is the next
/// by_array.
fn order_value(v: &CalcValue) -> Option<bool> {
    let n = match v {
        CalcValue::Blank => return Some(true),
        CalcValue::Number(n) => *n,
        _ => return None,
    };
    if n == 1.0 {
        Some(true)
    } else if n == -1.0 {
        Some(false)
    } else {
        None
    }
}

/// SORTBY(array, by_array1, [sort_order1], by_array2, [sort_order2], ...).
/// A stable multi-key sort: keys compare lexicographically in argument order.
fn sortby(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let array = to_array(args[0].value(ctx)?);
    let keys = sortby_keys(ctx, args, &array)?;
    let by_rows = array.rows > 1;
    let n = if by_rows { array.rows } else { array.cols };

    let mut idx: Vec<u32> = (0..n).collect();
    idx.sort_by(|&a, &b| {
        for (k, asc) in &keys {
            let o = sort_cmp(&k[a as usize], &k[b as usize]);
            let o = if *asc { o } else { o.reverse() };
            if o != Ordering::Equal {
                return o;
            }
        }
        Ordering::Equal
    });

    let (rows, cols) = if by_rows {
        (n, array.cols)
    } else {
        (array.rows, n)
    };
    let mut data = Vec::with_capacity((rows as usize).saturating_mul(cols as usize));
    for r in 0..rows {
        for c in 0..cols {
            if by_rows {
                data.push(array.get(idx[r as usize], c).clone());
            } else {
                data.push(array.get(r, idx[c as usize]).clone());
            }
        }
    }
    Ok(CalcValue::array(ArrayValue::new(rows, cols, data)))
}

// -- SEQUENCE ----------------------------------------------------------------

/// SEQUENCE(rows, [cols], [start], [step]). Fills across then down: element
/// (i, j) is `start + (i*cols + j)*step`.
fn sequence(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let rows = coerce_number(&args[0].value(ctx)?)?.trunc() as i64;
    let cols = if args.len() > 1 {
        coerce_number(&args[1].value(ctx)?)?.trunc() as i64
    } else {
        1
    };
    let start = if args.len() > 2 {
        coerce_number(&args[2].value(ctx)?)?
    } else {
        1.0
    };
    let step = if args.len() > 3 {
        coerce_number(&args[3].value(ctx)?)?
    } else {
        1.0
    };
    if rows < 0 || cols < 0 {
        return Err(CalcError::Value);
    }
    if rows == 0 || cols == 0 {
        return Err(CalcError::Calc);
    }
    let (rows, cols) = (rows as u32, cols as u32);
    check_cap(rows, cols)?;
    let mut data = Vec::with_capacity((rows as usize).saturating_mul(cols as usize));
    for i in 0..rows {
        for j in 0..cols {
            let v = start + (i as f64 * cols as f64 + j as f64) * step;
            if !v.is_finite() {
                return Err(CalcError::Num);
            }
            data.push(CalcValue::Number(v));
        }
    }
    Ok(CalcValue::array(ArrayValue::new(rows, cols, data)))
}

// -- RANDARRAY ---------------------------------------------------------------

/// SplitMix64 PRNG seeded from a process-global counter. Not cryptographic —
/// RANDARRAY needs variety between calls, not security.
static SEED: AtomicU64 = AtomicU64::new(0x4D59_5DF4_D0F3_3173);

fn next_u64() -> u64 {
    let mut z = SEED.fetch_add(0x9E37_79B9_7F4A_7C15, AtomicOrdering::Relaxed);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Uniform `[0, 1)`.
fn next_uniform() -> f64 {
    ((next_u64() >> 11) as f64) / ((1u64 << 53) as f64)
}

/// RANDARRAY([rows], [cols], [min], [max], [integer]). Volatile: registered
/// with `volatile: true` so its result is never cached. `integer` picks
/// integers in `[min, max]` inclusive; otherwise uniform in `[min, max)`.
fn randarray(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let rows = if !args.is_empty() {
        coerce_number(&args[0].value(ctx)?)?.trunc() as i64
    } else {
        1
    };
    let cols = if args.len() > 1 {
        coerce_number(&args[1].value(ctx)?)?.trunc() as i64
    } else {
        1
    };
    let min = if args.len() > 2 {
        coerce_number(&args[2].value(ctx)?)?
    } else {
        0.0
    };
    let max = if args.len() > 3 {
        coerce_number(&args[3].value(ctx)?)?
    } else {
        1.0
    };
    let integer = if args.len() > 4 {
        as_bool_arg(&args[4].value(ctx)?)?
    } else {
        false
    };
    if rows < 0 || cols < 0 {
        return Err(CalcError::Value);
    }
    if rows == 0 || cols == 0 {
        return Err(CalcError::Calc);
    }
    if min > max {
        return Err(CalcError::Value);
    }
    let (rows, cols) = (rows as u32, cols as u32);
    check_cap(rows, cols)?;
    let mut data = Vec::with_capacity((rows as usize).saturating_mul(cols as usize));
    for _ in 0..rows {
        for _ in 0..cols {
            let v = if integer {
                min + (next_uniform() * (max - min + 1.0)).floor()
            } else {
                min + next_uniform() * (max - min)
            };
            data.push(ok_num(v)?);
        }
    }
    Ok(CalcValue::array(ArrayValue::new(rows, cols, data)))
}

// -- XMATCH ------------------------------------------------------------------

/// XMATCH(lookup, lookup_array, [match_mode], [search_mode]). 1-based, like
/// MATCH; a 2-D lookup array is #VALUE!. When `lookup` is itself an array the
/// result is an array of positions (missing entries become #N/A).
fn xmatch(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let lookup = args[0].value(ctx)?;
    let array = to_array(args[1].value(ctx)?);
    let match_mode = match args.get(2) {
        Some(a) => {
            let v = a.value(ctx)?;
            if v.is_blank() {
                0
            } else {
                coerce_number(&v)? as i64
            }
        }
        None => 0,
    };
    let search_mode = match args.get(3) {
        Some(a) => {
            let v = a.value(ctx)?;
            if v.is_blank() {
                1
            } else {
                coerce_number(&v)? as i64
            }
        }
        None => 1,
    };
    if !matches!(match_mode, -1..=2) {
        return Err(CalcError::Value);
    }
    if !matches!(search_mode, 1 | -1 | 2 | -2) {
        return Err(CalcError::Value);
    }
    if array.rows != 1 && array.cols != 1 {
        return Err(CalcError::Value);
    }
    let values: Vec<CalcValue> = if array.cols == 1 {
        (0..array.rows).map(|r| array.get(r, 0).clone()).collect()
    } else {
        (0..array.cols).map(|c| array.get(0, c).clone()).collect()
    };

    match lookup {
        CalcValue::Array(l) => {
            check_cap(l.rows, l.cols)?;
            let mut data = Vec::with_capacity(l.data.len());
            for e in l.iter() {
                let pos = lookup_position(&values, e, match_mode, search_mode)?;
                data.push(match pos {
                    Some(i) => CalcValue::Number(i as f64 + 1.0),
                    None => CalcValue::Error(CalcError::Na),
                });
            }
            Ok(CalcValue::array(ArrayValue::new(l.rows, l.cols, data)))
        }
        other => match lookup_position(&values, &other, match_mode, search_mode)? {
            Some(i) => Ok(CalcValue::Number(i as f64 + 1.0)),
            None => Err(CalcError::Na),
        },
    }
}

// -- TOCOL / TOROW -----------------------------------------------------------

/// Keep-or-skip for TOCOL/TOROW's `ignore` code: 0 keep all, 1 drop blanks,
/// 2 drop errors, 3 drop both.
fn ignore_keeps(v: &CalcValue, ignore: i64) -> bool {
    match ignore {
        0 => true,
        1 => !v.is_blank(),
        2 => !matches!(v, CalcValue::Error(_)),
        _ => !v.is_blank() && !matches!(v, CalcValue::Error(_)),
    }
}

/// The `ignore` code, validated to 0..=3.
fn ignore_code(ctx: &FuncCtx, args: &[FuncArg]) -> Result<i64, CalcError> {
    if args.len() > 1 {
        let n = coerce_number(&args[1].value(ctx)?)?.trunc() as i64;
        if (0..=3).contains(&n) {
            Ok(n)
        } else {
            Err(CalcError::Value)
        }
    } else {
        Ok(0)
    }
}

/// The elements of `array` in TOCOL/TOROW scan order, with the `ignore` rule
/// applied. `by_col` scans column-by-column instead of row-by-row.
fn scan_elements(array: &ArrayValue, ignore: i64, by_col: bool) -> Vec<CalcValue> {
    let rows = array.rows as usize;
    let cols = array.cols as usize;
    let mut out: Vec<CalcValue> = Vec::new();
    for i in 0..array.data.len() {
        let (r, c) = if by_col {
            (i % rows, i / rows)
        } else {
            (i / cols, i % cols)
        };
        let v = array.get(r as u32, c as u32);
        if ignore_keeps(v, ignore) {
            out.push(v.clone());
        }
    }
    out
}

/// TOCOL(array, [ignore], [scan_by_column]): every element in scan order as a
/// single column. An empty result is #CALC!.
fn tocol(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let array = to_array(args[0].value(ctx)?);
    let ignore = ignore_code(ctx, args)?;
    let by_col = if args.len() > 2 {
        as_bool_arg(&args[2].value(ctx)?)?
    } else {
        false
    };
    let out = scan_elements(&array, ignore, by_col);
    if out.is_empty() {
        return Err(CalcError::Calc);
    }
    Ok(CalcValue::array(ArrayValue::new(out.len() as u32, 1, out)))
}

/// TOROW(array, [ignore], [scan_by_column]): every element in scan order as a
/// single row. An empty result is #CALC!.
fn torow(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let array = to_array(args[0].value(ctx)?);
    let ignore = ignore_code(ctx, args)?;
    let by_col = if args.len() > 2 {
        as_bool_arg(&args[2].value(ctx)?)?
    } else {
        false
    };
    let out = scan_elements(&array, ignore, by_col);
    if out.is_empty() {
        return Err(CalcError::Calc);
    }
    Ok(CalcValue::array(ArrayValue::new(1, out.len() as u32, out)))
}

// -- TAKE / DROP -------------------------------------------------------------

/// The slice TAKE keeps: positive `n` keeps from the start, negative keeps
/// from the end; the count clamps to the axis length. Returns (start, len).
fn take_slice(len: u32, n: i64) -> (u32, u32) {
    let take = n.unsigned_abs() as u32;
    let take = take.min(len);
    if n > 0 { (0, take) } else { (len - take, take) }
}

/// The slice DROP keeps: positive `n` discards from the start (keeps the
/// tail), negative discards from the end (keeps the head).
fn drop_slice(len: u32, n: i64) -> (u32, u32) {
    let drop = n.unsigned_abs() as u32;
    let drop = drop.min(len);
    if n > 0 {
        (drop, len - drop)
    } else {
        (0, len - drop)
    }
}

/// TAKE(array, rows, [cols]): rows/cols 0 or a fully empty result is #CALC!.
/// An omitted `cols` keeps every column.
fn take(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let array = to_array(args[0].value(ctx)?);
    let rows = coerce_number(&args[1].value(ctx)?)?.trunc() as i64;
    let cols = if args.len() > 2 {
        Some(coerce_number(&args[2].value(ctx)?)?.trunc() as i64)
    } else {
        None
    };
    if rows == 0 || cols == Some(0) {
        return Err(CalcError::Calc);
    }
    let (r0, rn) = take_slice(array.rows, rows);
    let (c0, cn) = match cols {
        Some(c) => take_slice(array.cols, c),
        None => (0, array.cols),
    };
    if rn == 0 || cn == 0 {
        return Err(CalcError::Calc);
    }
    check_cap(rn, cn)?;
    let mut data = Vec::with_capacity((rn as usize).saturating_mul(cn as usize));
    for r in r0..r0 + rn {
        for c in c0..c0 + cn {
            data.push(array.get(r, c).clone());
        }
    }
    Ok(CalcValue::array(ArrayValue::new(rn, cn, data)))
}

/// DROP(array, rows, [cols]): a zero count is #VALUE! (per the docs); dropping
/// everything is #CALC!. An omitted `cols` keeps every column.
fn drop_(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let array = to_array(args[0].value(ctx)?);
    let rows = coerce_number(&args[1].value(ctx)?)?.trunc() as i64;
    let cols = if args.len() > 2 {
        Some(coerce_number(&args[2].value(ctx)?)?.trunc() as i64)
    } else {
        None
    };
    if rows == 0 || cols == Some(0) {
        return Err(CalcError::Value);
    }
    let (r0, rn) = drop_slice(array.rows, rows);
    let (c0, cn) = match cols {
        Some(c) => drop_slice(array.cols, c),
        None => (0, array.cols),
    };
    if rn == 0 || cn == 0 {
        return Err(CalcError::Calc);
    }
    check_cap(rn, cn)?;
    let mut data = Vec::with_capacity((rn as usize).saturating_mul(cn as usize));
    for r in r0..r0 + rn {
        for c in c0..c0 + cn {
            data.push(array.get(r, c).clone());
        }
    }
    Ok(CalcValue::array(ArrayValue::new(rn, cn, data)))
}

// -- CHOOSECOLS / CHOOSEROWS -------------------------------------------------

/// One 1-based index, negatives counted from the end; 0 or out of range is
/// #VALUE!.
fn choose_index(v: &CalcValue, len: u32) -> Result<u32, CalcError> {
    let n = coerce_number(v)?.trunc() as i64;
    let idx = if n > 0 {
        if n > len as i64 {
            return Err(CalcError::Value);
        }
        n - 1
    } else if n < 0 {
        if n.abs() > len as i64 {
            return Err(CalcError::Value);
        }
        len as i64 + n
    } else {
        return Err(CalcError::Value);
    };
    Ok(idx as u32)
}

/// CHOOSECOLS(array, col_num1, [col_num2], ...). Index args may be arrays;
/// the chosen columns appear in the given order (duplicates allowed).
fn choosecols(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let array = to_array(args[0].value(ctx)?);
    let mut idxs: Vec<u32> = Vec::new();
    for arg in &args[1..] {
        match arg.value(ctx)? {
            CalcValue::Array(a) => {
                for e in a.iter() {
                    idxs.push(choose_index(e, array.cols)?);
                }
            }
            other => idxs.push(choose_index(&other, array.cols)?),
        }
    }
    if idxs.is_empty() {
        return Err(CalcError::Value);
    }
    check_cap(array.rows, idxs.len() as u32)?;
    let mut data = Vec::with_capacity((array.rows as usize).saturating_mul(idxs.len()));
    for r in 0..array.rows {
        for &c in &idxs {
            data.push(array.get(r, c).clone());
        }
    }
    Ok(CalcValue::array(ArrayValue::new(
        array.rows,
        idxs.len() as u32,
        data,
    )))
}

/// CHOOSEROWS(array, row_num1, [row_num2], ...), the row analogue.
fn chooserows(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let array = to_array(args[0].value(ctx)?);
    let mut idxs: Vec<u32> = Vec::new();
    for arg in &args[1..] {
        match arg.value(ctx)? {
            CalcValue::Array(a) => {
                for e in a.iter() {
                    idxs.push(choose_index(e, array.rows)?);
                }
            }
            other => idxs.push(choose_index(&other, array.rows)?),
        }
    }
    if idxs.is_empty() {
        return Err(CalcError::Value);
    }
    check_cap(idxs.len() as u32, array.cols)?;
    let mut data = Vec::with_capacity((array.cols as usize).saturating_mul(idxs.len()));
    for &r in &idxs {
        for c in 0..array.cols {
            data.push(array.get(r, c).clone());
        }
    }
    Ok(CalcValue::array(ArrayValue::new(
        idxs.len() as u32,
        array.cols,
        data,
    )))
}

// -- EXPAND ------------------------------------------------------------------

/// EXPAND(array, rows, [cols], [pad_with]): enlarge to `rows` x `cols`,
/// padding the new cells with `pad_with` (default #N/A). Shrinking either
/// dimension is #VALUE!.
fn expand(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let array = to_array(args[0].value(ctx)?);
    let rows = coerce_number(&args[1].value(ctx)?)?.trunc() as i64;
    let cols = if args.len() > 2 {
        coerce_number(&args[2].value(ctx)?)?.trunc() as i64
    } else {
        array.cols as i64
    };
    let pad = if args.len() > 3 {
        args[3].value(ctx)?
    } else {
        CalcValue::err(CalcError::Na)
    };
    if rows < array.rows as i64 || cols < array.cols as i64 || rows <= 0 || cols <= 0 {
        return Err(CalcError::Value);
    }
    let (rows, cols) = (rows as u32, cols as u32);
    check_cap(rows, cols)?;
    let mut data = Vec::with_capacity((rows as usize).saturating_mul(cols as usize));
    for r in 0..rows {
        for c in 0..cols {
            if r < array.rows && c < array.cols {
                data.push(array.get(r, c).clone());
            } else {
                data.push(pad.clone());
            }
        }
    }
    Ok(CalcValue::array(ArrayValue::new(rows, cols, data)))
}

// -- HSTACK / VSTACK ---------------------------------------------------------

/// HSTACK(array1, [array2], ...): concatenate side by side, padding shorter
/// arrays with #N/A so every column line has the same height.
fn hstack(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let mut arrays: Vec<ArrayValue> = Vec::with_capacity(args.len());
    for a in args {
        arrays.push(to_array(a.value(ctx)?));
    }
    let rows = arrays.iter().map(|a| a.rows).max().unwrap_or(0);
    let cols: u32 = arrays.iter().map(|a| a.cols).sum();
    check_cap(rows, cols)?;
    let na = CalcValue::err(CalcError::Na);
    let mut data = Vec::with_capacity((rows as usize).saturating_mul(cols as usize));
    for r in 0..rows {
        for a in &arrays {
            for c in 0..a.cols {
                if r < a.rows {
                    data.push(a.get(r, c).clone());
                } else {
                    data.push(na.clone());
                }
            }
        }
    }
    Ok(CalcValue::array(ArrayValue::new(rows, cols, data)))
}

/// VSTACK(array1, [array2], ...): concatenate one below the other, padding
/// narrower arrays with #N/A.
fn vstack(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let mut arrays: Vec<ArrayValue> = Vec::with_capacity(args.len());
    for a in args {
        arrays.push(to_array(a.value(ctx)?));
    }
    let cols = arrays.iter().map(|a| a.cols).max().unwrap_or(0);
    let rows: u32 = arrays.iter().map(|a| a.rows).sum();
    check_cap(rows, cols)?;
    let na = CalcValue::err(CalcError::Na);
    let mut data = Vec::with_capacity((rows as usize).saturating_mul(cols as usize));
    for a in &arrays {
        for r in 0..a.rows {
            for c in 0..cols {
                if c < a.cols {
                    data.push(a.get(r, c).clone());
                } else {
                    data.push(na.clone());
                }
            }
        }
    }
    Ok(CalcValue::array(ArrayValue::new(rows, cols, data)))
}

// -- TEXTSPLIT ---------------------------------------------------------------

/// Split on `sep`, keeping empty segments (Rust's `str::split` already does);
/// with `ignore_empty` the empties are dropped. Never fails on multi-char
/// separators.
fn split_segments(s: &str, sep: &str, ignore_empty: bool) -> Vec<String> {
    s.split(sep)
        .map(|p| p.to_string())
        .filter(|p| !ignore_empty || !p.is_empty())
        .collect()
}

/// TEXTSPLIT(text, col_delimiter, [row_delimiter], [ignore_empty],
/// [pad_with]). Either delimiter may be blank to mean "no split on that axis"
/// (at least one must be present); ragged rows are padded with `pad_with`
/// (default #N/A); an empty result is #CALC!.
fn textsplit(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let text = coerce_text(&args[0].value(ctx)?)?;
    let col_delim = coerce_text(&args[1].value(ctx)?)?;
    let row_delim = if args.len() > 2 {
        coerce_text(&args[2].value(ctx)?)?
    } else {
        String::new()
    };
    let ignore_empty = if args.len() > 3 {
        as_bool_arg(&args[3].value(ctx)?)?
    } else {
        false
    };
    let pad = if args.len() > 4 {
        args[4].value(ctx)?
    } else {
        CalcValue::err(CalcError::Na)
    };

    let split_cols = !col_delim.is_empty();
    let split_rows = !row_delim.is_empty();
    if !split_cols && !split_rows {
        return Err(CalcError::Value);
    }

    let row_strs: Vec<String> = if split_rows {
        split_segments(&text, &row_delim, ignore_empty)
    } else {
        vec![text]
    };
    let mut grid: Vec<Vec<CalcValue>> = Vec::with_capacity(row_strs.len());
    let mut cols = 0usize;
    for row in &row_strs {
        let cells: Vec<CalcValue> = if split_cols {
            split_segments(row, &col_delim, ignore_empty)
                .into_iter()
                .map(CalcValue::text)
                .collect()
        } else {
            vec![CalcValue::text(row.clone())]
        };
        cols = cols.max(cells.len());
        grid.push(cells);
    }
    if grid.is_empty() || cols == 0 {
        return Err(CalcError::Calc);
    }
    let rows = grid.len() as u32;
    check_cap(rows, cols as u32)?;
    let mut data = Vec::with_capacity((rows as usize).saturating_mul(cols));
    for cells in &grid {
        for i in 0..cols {
            if i < cells.len() {
                data.push(cells[i].clone());
            } else {
                data.push(pad.clone());
            }
        }
    }
    Ok(CalcValue::array(ArrayValue::new(rows, cols as u32, data)))
}

// -- registration ------------------------------------------------------------

/// Implemented and fully tested, but deliberately NOT registered — the lookup
/// family already owns the name and a duplicate registration trips the
/// registry's `debug_assert` (panic in the debug test build). The coordinator
/// removes lookup.rs's XLOOKUP, then registers this one.
#[allow(dead_code)]
const XLOOKUP: FuncSpec = FuncSpec {
    name: "XLOOKUP",
    min_args: 3,
    max_args: Some(6),
    volatile: false,
    array_aware: true,
    func: xlookup,
};

const FILTER: FuncSpec = FuncSpec {
    name: "FILTER",
    min_args: 2,
    max_args: Some(3),
    volatile: false,
    array_aware: true,
    func: filter,
};

const UNIQUE: FuncSpec = FuncSpec {
    name: "UNIQUE",
    min_args: 1,
    max_args: Some(3),
    volatile: false,
    array_aware: true,
    func: unique,
};

const SORT: FuncSpec = FuncSpec {
    name: "SORT",
    min_args: 1,
    max_args: Some(4),
    volatile: false,
    array_aware: true,
    func: sort,
};

const SORTBY: FuncSpec = FuncSpec {
    name: "SORTBY",
    min_args: 2,
    max_args: None,
    volatile: false,
    array_aware: true,
    func: sortby,
};

const SEQUENCE: FuncSpec = FuncSpec {
    name: "SEQUENCE",
    min_args: 1,
    max_args: Some(4),
    volatile: false,
    array_aware: true,
    func: sequence,
};

const RANDARRAY: FuncSpec = FuncSpec {
    name: "RANDARRAY",
    min_args: 0,
    max_args: Some(5),
    volatile: true,
    array_aware: true,
    func: randarray,
};

const XMATCH: FuncSpec = FuncSpec {
    name: "XMATCH",
    min_args: 2,
    max_args: Some(4),
    volatile: false,
    array_aware: true,
    func: xmatch,
};

const TOCOL: FuncSpec = FuncSpec {
    name: "TOCOL",
    min_args: 1,
    max_args: Some(3),
    volatile: false,
    array_aware: true,
    func: tocol,
};

const TOROW: FuncSpec = FuncSpec {
    name: "TOROW",
    min_args: 1,
    max_args: Some(3),
    volatile: false,
    array_aware: true,
    func: torow,
};

const TAKE: FuncSpec = FuncSpec {
    name: "TAKE",
    min_args: 2,
    max_args: Some(3),
    volatile: false,
    array_aware: true,
    func: take,
};

const DROP: FuncSpec = FuncSpec {
    name: "DROP",
    min_args: 2,
    max_args: Some(3),
    volatile: false,
    array_aware: true,
    func: drop_,
};

const CHOOSECOLS: FuncSpec = FuncSpec {
    name: "CHOOSECOLS",
    min_args: 2,
    max_args: Some(254),
    volatile: false,
    array_aware: true,
    func: choosecols,
};

const CHOOSEROWS: FuncSpec = FuncSpec {
    name: "CHOOSEROWS",
    min_args: 2,
    max_args: Some(254),
    volatile: false,
    array_aware: true,
    func: chooserows,
};

const EXPAND: FuncSpec = FuncSpec {
    name: "EXPAND",
    min_args: 2,
    max_args: Some(4),
    volatile: false,
    array_aware: true,
    func: expand,
};

const HSTACK: FuncSpec = FuncSpec {
    name: "HSTACK",
    min_args: 1,
    max_args: Some(254),
    volatile: false,
    array_aware: true,
    func: hstack,
};

const VSTACK: FuncSpec = FuncSpec {
    name: "VSTACK",
    min_args: 1,
    max_args: Some(254),
    volatile: false,
    array_aware: true,
    func: vstack,
};

const TEXTSPLIT: FuncSpec = FuncSpec {
    name: "TEXTSPLIT",
    min_args: 2,
    max_args: Some(5),
    volatile: false,
    array_aware: true,
    func: textsplit,
};

pub fn register(r: &mut Registry) {
    // XLOOKUP lives here, not in functions/lookup.rs, because it is a
    // dynamic-array function: its `return_array` may be wider than one column,
    // in which case it returns a block that spills. lookup.rs's version only
    // ever returns a single value. Its registration has been removed by the
    // coordinator and its const kept as an independent cross-check — see the
    // comment at its `register` site.
    r.register(&XLOOKUP);
    r.register(&FILTER);
    r.register(&UNIQUE);
    r.register(&SORT);
    r.register(&SORTBY);
    r.register(&SEQUENCE);
    r.register(&RANDARRAY);
    r.register(&XMATCH);
    r.register(&TOCOL);
    r.register(&TOROW);
    r.register(&TAKE);
    r.register(&DROP);
    r.register(&CHOOSECOLS);
    r.register(&CHOOSEROWS);
    r.register(&EXPAND);
    r.register(&HSTACK);
    r.register(&VSTACK);
    r.register(&TEXTSPLIT);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbo::calc::functions::{CellResolver, Func};
    use crate::turbo::calc::testkit::{Grid, Outcome};
    use pretty_assertions::assert_eq;

    struct EmptyResolver;
    impl CellResolver for EmptyResolver {
        fn cell(&self, _sheet: u32, _row: u32, _col: u32) -> Option<CalcValue> {
            None
        }
        fn sheet_index(&self, _name: &str) -> Option<u32> {
            None
        }
    }

    fn call(f: Func, vals: Vec<CalcValue>) -> Result<CalcValue, CalcError> {
        static R: EmptyResolver = EmptyResolver;
        let c = FuncCtx {
            date1904: false,
            sheet: 0,
            row: 0,
            col: 0,
            resolver: &R,
        };
        let args: Vec<FuncArg> = vals.into_iter().map(FuncArg::Value).collect();
        f(&c, &args)
    }

    fn arr(rows: u32, cols: u32, data: Vec<CalcValue>) -> CalcValue {
        CalcValue::array(ArrayValue::new(rows, cols, data))
    }
    fn col(data: Vec<CalcValue>) -> CalcValue {
        arr(data.len() as u32, 1, data)
    }
    fn row(data: Vec<CalcValue>) -> CalcValue {
        arr(1, data.len() as u32, data)
    }
    fn num(n: f64) -> CalcValue {
        CalcValue::Number(n)
    }
    fn txt(s: &str) -> CalcValue {
        CalcValue::text(s)
    }
    fn na() -> CalcValue {
        CalcValue::err(CalcError::Na)
    }

    fn shape_of(v: &CalcValue) -> (u32, u32) {
        match v {
            CalcValue::Array(a) => a.shape(),
            other => panic!("expected array, got {other:?}"),
        }
    }

    /// Every function this file ships must be live in the registry and
    /// array-aware (the eval loop must hand it arrays untouched). RANDARRAY is
    /// additionally volatile so its result is never cached.
    #[test]
    fn every_registered_dynamic_function_is_array_aware() {
        use crate::turbo::calc::functions::registry;
        for name in [
            "FILTER",
            "UNIQUE",
            "SORT",
            "SORTBY",
            "SEQUENCE",
            "RANDARRAY",
            "XMATCH",
            "TOCOL",
            "TOROW",
            "TAKE",
            "DROP",
            "CHOOSECOLS",
            "CHOOSEROWS",
            "EXPAND",
            "HSTACK",
            "VSTACK",
            "TEXTSPLIT",
        ] {
            let spec = registry()
                .get(name)
                .unwrap_or_else(|| panic!("{name} not registered"));
            assert!(spec.array_aware, "{name} must be array-aware");
        }
        let spec = registry().get("RANDARRAY").unwrap();
        assert!(spec.volatile, "RANDARRAY must never be cached");
    }

    // -- XLOOKUP (direct calls; the name is owned by lookup.rs until the
    // coordinator merges — see the module doc) --------------------------------

    #[test]
    fn xlookup_exact_mode_and_defaults() {
        let look = col(vec![num(1.0), num(2.0), num(3.0)]);
        let ret = col(vec![txt("a"), txt("b"), txt("c")]);
        assert_eq!(
            call(xlookup, vec![num(2.0), look.clone(), ret.clone()]),
            Ok(txt("b"))
        );
        // no match and no if_not_found -> #N/A
        assert_eq!(
            call(xlookup, vec![num(9.0), look.clone(), ret.clone()]),
            Err(CalcError::Na)
        );
        // if_not_found replaces it
        assert_eq!(
            call(
                xlookup,
                vec![num(9.0), look.clone(), ret.clone(), txt("nf")]
            ),
            Ok(txt("nf"))
        );
        // a scalar lookup/return pair works through 1x1 wrapping
        assert_eq!(
            call(xlookup, vec![num(5.0), num(5.0), num(50.0)]),
            Ok(num(50.0))
        );
    }

    #[test]
    fn xlookup_approximate_match_modes() {
        let look = col(vec![num(1.0), num(2.0), num(3.0), num(5.0)]);
        let ret = col(vec![txt("one"), txt("two"), txt("three"), txt("five")]);
        // -1 next smaller
        assert_eq!(
            call(
                xlookup,
                vec![
                    num(4.0),
                    look.clone(),
                    ret.clone(),
                    CalcValue::Blank,
                    num(-1.0)
                ]
            ),
            Ok(txt("three"))
        );
        // 1 next larger
        assert_eq!(
            call(
                xlookup,
                vec![
                    num(4.0),
                    look.clone(),
                    ret.clone(),
                    CalcValue::Blank,
                    num(1.0)
                ]
            ),
            Ok(txt("five"))
        );
        // 2 wildcard
        let words = col(vec![txt("apple"), txt("apricot"), txt("banana")]);
        let ids = col(vec![num(10.0), num(20.0), num(30.0)]);
        assert_eq!(
            call(
                xlookup,
                vec![
                    txt("ap*"),
                    words.clone(),
                    ids.clone(),
                    CalcValue::Blank,
                    num(2.0)
                ]
            ),
            Ok(num(10.0))
        );
        assert_eq!(
            call(
                xlookup,
                vec![txt("*c*"), words, ids, CalcValue::Blank, num(2.0)]
            ),
            Ok(num(20.0))
        );
    }

    #[test]
    fn xlookup_search_modes_first_and_last() {
        let look = col(vec![num(1.0), num(2.0), num(2.0), num(3.0)]);
        let ret = col(vec![num(10.0), num(20.0), num(21.0), num(30.0)]);
        // 1 first-to-last
        assert_eq!(
            call(
                xlookup,
                vec![
                    num(2.0),
                    look.clone(),
                    ret.clone(),
                    CalcValue::Blank,
                    num(0.0),
                    num(1.0)
                ]
            ),
            Ok(num(20.0))
        );
        // -1 last-to-first
        assert_eq!(
            call(
                xlookup,
                vec![
                    num(2.0),
                    look.clone(),
                    ret.clone(),
                    CalcValue::Blank,
                    num(0.0),
                    num(-1.0)
                ]
            ),
            Ok(num(21.0))
        );
    }

    #[test]
    fn xlookup_binary_search_ascending_and_descending() {
        let asc = col(vec![num(1.0), num(3.0), num(5.0), num(7.0)]);
        let ret = col(vec![num(10.0), num(30.0), num(50.0), num(70.0)]);
        // exact via binary ascending
        assert_eq!(
            call(
                xlookup,
                vec![
                    num(5.0),
                    asc.clone(),
                    ret.clone(),
                    CalcValue::Blank,
                    num(0.0),
                    num(2.0)
                ]
            ),
            Ok(num(50.0))
        );
        // next smaller via binary ascending: 6 -> 5
        assert_eq!(
            call(
                xlookup,
                vec![
                    num(6.0),
                    asc.clone(),
                    ret.clone(),
                    CalcValue::Blank,
                    num(-1.0),
                    num(2.0)
                ]
            ),
            Ok(num(50.0))
        );
        // next larger via binary ascending: 6 -> 7
        assert_eq!(
            call(
                xlookup,
                vec![num(6.0), asc, ret, CalcValue::Blank, num(1.0), num(2.0)]
            ),
            Ok(num(70.0))
        );

        let desc = col(vec![num(7.0), num(5.0), num(3.0), num(1.0)]);
        let dr = col(vec![num(70.0), num(50.0), num(30.0), num(10.0)]);
        // exact via binary descending
        assert_eq!(
            call(
                xlookup,
                vec![
                    num(5.0),
                    desc.clone(),
                    dr.clone(),
                    CalcValue::Blank,
                    num(0.0),
                    num(-2.0)
                ]
            ),
            Ok(num(50.0))
        );
        // next smaller (largest <= 6 in descending data is 5)
        assert_eq!(
            call(
                xlookup,
                vec![
                    num(6.0),
                    desc.clone(),
                    dr.clone(),
                    CalcValue::Blank,
                    num(-1.0),
                    num(-2.0)
                ]
            ),
            Ok(num(50.0))
        );
        // next larger (smallest >= 6 is 7)
        assert_eq!(
            call(
                xlookup,
                vec![num(6.0), desc, dr, CalcValue::Blank, num(1.0), num(-2.0)]
            ),
            Ok(num(70.0))
        );
    }

    #[test]
    fn xlookup_block_return_for_wide_return_array() {
        let look = col(vec![num(1.0), num(2.0), num(3.0)]);
        let ret = arr(
            3,
            2,
            vec![
                num(10.0),
                num(11.0),
                num(20.0),
                num(21.0),
                num(30.0),
                num(31.0),
            ],
        );
        match call(xlookup, vec![num(2.0), look, ret]) {
            Ok(v) => {
                assert_eq!(shape_of(&v), (1, 2));
                assert_eq!(v, arr(1, 2, vec![num(20.0), num(21.0)]));
            }
            other => panic!("expected a 1x2 row, got {other:?}"),
        }
        // row lookup with a tall return gives a column: match 2 -> column 2
        let rlook = row(vec![num(1.0), num(2.0), num(3.0)]);
        let rret = arr(
            2,
            3,
            vec![
                num(10.0),
                num(11.0),
                num(12.0),
                num(20.0),
                num(21.0),
                num(22.0),
            ],
        );
        match call(xlookup, vec![num(2.0), rlook, rret]) {
            Ok(v) => assert_eq!(v, arr(2, 1, vec![num(11.0), num(21.0)])),
            other => panic!("expected a 2x1 column, got {other:?}"),
        }
    }

    #[test]
    fn xlookup_errors() {
        let look = col(vec![num(1.0), num(2.0), num(3.0)]);
        let ret = col(vec![num(10.0), num(20.0), num(30.0)]);
        // invalid match_mode
        assert_eq!(
            call(
                xlookup,
                vec![
                    num(1.0),
                    look.clone(),
                    ret.clone(),
                    CalcValue::Blank,
                    num(5.0)
                ]
            ),
            Err(CalcError::Value)
        );
        // non-numeric match_mode
        assert_eq!(
            call(
                xlookup,
                vec![
                    num(1.0),
                    look.clone(),
                    ret.clone(),
                    CalcValue::Blank,
                    txt("abc")
                ]
            ),
            Err(CalcError::Value)
        );
        // invalid search_mode (0 is not legal)
        assert_eq!(
            call(
                xlookup,
                vec![
                    num(1.0),
                    look.clone(),
                    ret.clone(),
                    CalcValue::Blank,
                    num(0.0),
                    num(0.0)
                ]
            ),
            Err(CalcError::Value)
        );
        // length mismatch between lookup and return arrays
        assert_eq!(
            call(xlookup, vec![num(1.0), look.clone(), col(vec![num(1.0)])]),
            Err(CalcError::Value)
        );
        // a 2-D lookup array is #VALUE!
        let two_d = arr(2, 2, vec![num(1.0), num(2.0), num(3.0), num(4.0)]);
        assert_eq!(
            call(xlookup, vec![num(1.0), two_d, ret.clone()]),
            Err(CalcError::Value)
        );
        // omitted if_not_found with no match is #N/A
        assert_eq!(call(xlookup, vec![num(9.0), look, ret]), Err(CalcError::Na));
    }

    // -- FILTER ---------------------------------------------------------------

    #[test]
    fn filter_keeps_rows_where_include_is_true() {
        let g = Grid::empty()
            .set_num("A1", 1.0)
            .set_num("A2", 2.0)
            .set_num("A3", 3.0)
            .set_num("A4", 4.0)
            .set_bool("B1", true)
            .set_bool("B2", false)
            .set_bool("B3", true)
            .set_bool("B4", false);
        assert_eq!(
            g.array("=FILTER(A1:A4, B1:B4)"),
            ArrayValue::new(2, 1, vec![num(1.0), num(3.0)])
        );
        assert_eq!(
            g.array("=FILTER(A1:A4, A1:A4>2)"),
            ArrayValue::new(2, 1, vec![num(3.0), num(4.0)])
        );
        // filter columns through a row of booleans
        let g2 = Grid::empty().row("A1", &[1.0, 2.0, 3.0]);
        assert_eq!(
            g2.array("=FILTER(A1:C1, {TRUE,FALSE,TRUE})"),
            ArrayValue::new(1, 2, vec![num(1.0), num(3.0)])
        );
    }

    #[test]
    fn filter_empty_result_is_calc_unless_if_empty() {
        let g = Grid::empty().col("A1", &[1.0, 2.0, 3.0]);
        assert_eq!(g.error("=FILTER(A1:A3, A1:A3>10)"), CalcError::Calc);
        assert_eq!(
            g.calc("=FILTER(A1:A3, A1:A3>10, \"none\")"),
            Outcome::Value(txt("none"))
        );
    }

    #[test]
    fn filter_shape_mismatch_is_value() {
        let g = Grid::empty().col("A1", &[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(g.error("=FILTER(A1:A4, A1:A3)"), CalcError::Value);
    }

    // -- UNIQUE ---------------------------------------------------------------

    #[test]
    fn unique_distinct_and_exactly_once() {
        let g = Grid::empty().col("A1", &[1.0, 2.0, 2.0, 3.0, 1.0]);
        assert_eq!(
            g.array("=UNIQUE(A1:A5)"),
            ArrayValue::new(3, 1, vec![num(1.0), num(2.0), num(3.0)])
        );
        assert_eq!(
            g.array("=UNIQUE(A1:A5, FALSE, TRUE)"),
            ArrayValue::new(1, 1, vec![num(3.0)])
        );
    }

    #[test]
    fn unique_rows_and_columns() {
        let g = Grid::empty()
            .set_num("A1", 1.0)
            .set_text("B1", "x")
            .set_num("A2", 1.0)
            .set_text("B2", "x")
            .set_num("A3", 2.0)
            .set_text("B3", "y");
        assert_eq!(
            g.array("=UNIQUE(A1:B3)"),
            ArrayValue::new(2, 2, vec![num(1.0), txt("x"), num(2.0), txt("y")])
        );
        // duplicate columns collapse under by_col
        let g2 = Grid::empty().row("A1", &[5.0, 5.0, 7.0]);
        assert_eq!(
            g2.array("=UNIQUE(A1:C1, TRUE)"),
            ArrayValue::new(1, 2, vec![num(5.0), num(7.0)])
        );
    }

    // -- SORT -----------------------------------------------------------------

    #[test]
    fn sort_ascending_descending_and_by_col() {
        let g = Grid::empty()
            .set_num("A1", 3.0)
            .set_text("B1", "c")
            .set_num("A2", 1.0)
            .set_text("B2", "a")
            .set_num("A3", 2.0)
            .set_text("B3", "b");
        assert_eq!(
            g.array("=SORT(A1:B3)"),
            ArrayValue::new(
                3,
                2,
                vec![num(1.0), txt("a"), num(2.0), txt("b"), num(3.0), txt("c")]
            )
        );
        assert_eq!(
            g.array("=SORT(A1:B3, 1, -1)"),
            ArrayValue::new(
                3,
                2,
                vec![num(3.0), txt("c"), num(2.0), txt("b"), num(1.0), txt("a")]
            )
        );
        let wide = Grid::empty().row("A1", &[3.0, 1.0, 2.0]);
        assert_eq!(
            wide.array("=SORT(A1:C1, 1, 1, TRUE)"),
            ArrayValue::new(1, 3, vec![num(1.0), num(2.0), num(3.0)])
        );
    }

    #[test]
    fn sort_is_stable() {
        let g = Grid::empty()
            .set_num("A1", 2.0)
            .set_text("B1", "a")
            .set_num("A2", 1.0)
            .set_text("B2", "b")
            .set_num("A3", 2.0)
            .set_text("B3", "c")
            .set_num("A4", 1.0)
            .set_text("B4", "d");
        // descending keeps the original relative order of equal keys: 2a,2c,1b,1d
        assert_eq!(
            g.array("=SORT(A1:B4, 1, -1)"),
            ArrayValue::new(
                4,
                2,
                vec![
                    num(2.0),
                    txt("a"),
                    num(2.0),
                    txt("c"),
                    num(1.0),
                    txt("b"),
                    num(1.0),
                    txt("d"),
                ]
            )
        );
    }

    #[test]
    fn sort_out_of_range_index_is_value() {
        let g = Grid::empty().col("A1", &[1.0, 2.0, 3.0]);
        assert_eq!(g.error("=SORT(A1:A3, 2)"), CalcError::Value);
        assert_eq!(g.error("=SORT(A1:A3, 1, 0)"), CalcError::Value);
    }

    // -- SORTBY ---------------------------------------------------------------

    #[test]
    fn sortby_sorts_by_another_array() {
        let g = Grid::empty()
            .set_num("A1", 1.0)
            .set_text("B1", "z")
            .set_num("A2", 3.0)
            .set_text("B2", "x")
            .set_num("A3", 2.0)
            .set_text("B3", "y");
        assert_eq!(
            g.array("=SORTBY(A1:B3, B1:B3)"),
            ArrayValue::new(
                3,
                2,
                vec![num(3.0), txt("x"), num(2.0), txt("y"), num(1.0), txt("z")]
            )
        );
        assert_eq!(
            g.array("=SORTBY(A1:B3, B1:B3, -1)"),
            ArrayValue::new(
                3,
                2,
                vec![num(1.0), txt("z"), num(2.0), txt("y"), num(3.0), txt("x")]
            )
        );
    }

    #[test]
    fn sortby_multi_key_is_lexicographic_and_stable() {
        let g = Grid::empty()
            .set_num("A1", 2.0)
            .set_text("B1", "a")
            .set_num("A2", 1.0)
            .set_text("B2", "b")
            .set_num("A3", 1.0)
            .set_text("B3", "c")
            .set_num("A4", 1.0)
            .set_text("B4", "a");
        assert_eq!(
            g.array("=SORTBY(A1:B4, A1:A4, 1, B1:B4, 1)"),
            ArrayValue::new(
                4,
                2,
                vec![
                    num(1.0),
                    txt("a"),
                    num(1.0),
                    txt("b"),
                    num(1.0),
                    txt("c"),
                    num(2.0),
                    txt("a"),
                ]
            )
        );
    }

    #[test]
    fn sortby_length_mismatch_is_value() {
        let g = Grid::empty().col("A1", &[1.0, 2.0, 3.0]);
        assert_eq!(g.error("=SORTBY(A1:A3, A1:A2)"), CalcError::Value);
    }

    // -- SEQUENCE -------------------------------------------------------------

    #[test]
    fn sequence_fills_across_then_down() {
        assert_eq!(
            Grid::empty().array("=SEQUENCE(2, 3)"),
            ArrayValue::new(
                2,
                3,
                vec![num(1.0), num(2.0), num(3.0), num(4.0), num(5.0), num(6.0)]
            )
        );
        assert_eq!(
            Grid::empty().array("=SEQUENCE(3, 1, 10, 5)"),
            ArrayValue::new(3, 1, vec![num(10.0), num(15.0), num(20.0)])
        );
    }

    #[test]
    fn sequence_zero_or_negative_is_error() {
        assert_eq!(Grid::empty().error("=SEQUENCE(0, 3)"), CalcError::Calc);
        assert_eq!(Grid::empty().error("=SEQUENCE(-1)"), CalcError::Value);
    }

    // -- RANDARRAY ------------------------------------------------------------

    #[test]
    fn randarray_shape_and_range() {
        let a = Grid::empty().array("=RANDARRAY(2, 3)");
        assert_eq!(a.shape(), (2, 3));
        for v in a.iter() {
            let n = v.as_number().expect("random value is a number");
            assert!((0.0..1.0).contains(&n), "uniform in [0,1), got {n}");
        }
        let one = Grid::empty().array("=RANDARRAY()");
        assert_eq!(one.shape(), (1, 1));
        let ints = Grid::empty().array("=RANDARRAY(1, 1, 1, 5, TRUE)");
        let n = ints.get(0, 0).as_number().unwrap();
        assert!((1.0..=5.0).contains(&n), "integer in [1,5], got {n}");
    }

    #[test]
    fn randarray_errors_and_volatility() {
        assert_eq!(
            Grid::empty().error("=RANDARRAY(1, 1, 5, 1)"),
            CalcError::Value
        );
        assert_eq!(Grid::empty().error("=RANDARRAY(0)"), CalcError::Calc);
        assert_eq!(Grid::empty().error("=RANDARRAY(1, 0)"), CalcError::Calc);
        assert_eq!(Grid::empty().error("=RANDARRAY(-1)"), CalcError::Value);
        let spec = crate::turbo::calc::functions::registry()
            .get("RANDARRAY")
            .unwrap();
        assert!(spec.volatile, "RANDARRAY must never be cached");
    }

    // -- XMATCH ---------------------------------------------------------------

    #[test]
    fn xmatch_modes_and_search_directions() {
        let g = Grid::empty().col("A1", &[1.0, 2.0, 3.0, 5.0]);
        assert_eq!(g.num("=XMATCH(3, A1:A4)"), 3.0);
        assert_eq!(g.num("=XMATCH(4, A1:A4, -1)"), 3.0);
        assert_eq!(g.num("=XMATCH(4, A1:A4, 1)"), 4.0);
        assert_eq!(g.num("=XMATCH(1, A1:A4, 0, -1)"), 1.0);
        assert_eq!(g.num("=XMATCH(5, {1,3,5,7}, 0, 2)"), 3.0);
        assert_eq!(g.num("=XMATCH(5, {7,5,3,1}, 0, -2)"), 2.0);
        assert_eq!(g.num("=XMATCH(\"ap*\", {\"apple\",\"banana\"}, 2)"), 1.0);
    }

    #[test]
    fn xmatch_not_found_and_array_lookup() {
        let g = Grid::empty().col("A1", &[1.0, 2.0, 3.0]);
        assert_eq!(g.error("=XMATCH(9, A1:A3)"), CalcError::Na);
        assert_eq!(
            g.array("=XMATCH({2,3}, A1:A3)"),
            ArrayValue::new(1, 2, vec![num(2.0), num(3.0)])
        );
        assert_eq!(g.error("=XMATCH(1, A1:A3, 0, 0)"), CalcError::Value);
    }

    // -- TOCOL / TOROW --------------------------------------------------------

    #[test]
    fn tocol_scan_order_and_ignore() {
        let g = Grid::empty().row("A1", &[1.0, 2.0, 3.0]);
        let g = g.row("A2", &[4.0, 5.0, 6.0]);
        assert_eq!(
            g.array("=TOCOL(A1:C2)"),
            ArrayValue::new(
                6,
                1,
                vec![num(1.0), num(2.0), num(3.0), num(4.0), num(5.0), num(6.0)]
            )
        );
        // scan by column first
        assert_eq!(
            g.array("=TOCOL(A1:C2, , TRUE)"),
            ArrayValue::new(
                6,
                1,
                vec![num(1.0), num(4.0), num(2.0), num(5.0), num(3.0), num(6.0)]
            )
        );
        // ignore blanks (B2 left empty)
        let gb = Grid::empty()
            .set_num("A1", 1.0)
            .set_num("B1", 2.0)
            .set_num("A2", 4.0);
        assert_eq!(
            gb.array("=TOCOL(A1:B2, 1)"),
            ArrayValue::new(3, 1, vec![num(1.0), num(2.0), num(4.0)])
        );
        assert_eq!(
            gb.array("=TOROW(A1:B2, 1)"),
            ArrayValue::new(1, 3, vec![num(1.0), num(2.0), num(4.0)])
        );
    }

    #[test]
    fn tocol_ignore_errors_and_empty_result() {
        let g = Grid::empty().set_num("A1", 1.0).set("A2", na());
        assert_eq!(
            g.array("=TOCOL(A1:A2, 2)"),
            ArrayValue::new(1, 1, vec![num(1.0)])
        );
        assert_eq!(g.error("=TOCOL({#N/A}, 2)"), CalcError::Calc);
        assert_eq!(g.error("=TOCOL(A1:A2, 5)"), CalcError::Value);
    }

    // -- TAKE / DROP ----------------------------------------------------------

    #[test]
    fn take_and_drop_positive_and_negative() {
        let g = Grid::empty().col("A1", &[1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(
            g.array("=TAKE(A1:A5, 3)"),
            ArrayValue::new(3, 1, vec![num(1.0), num(2.0), num(3.0)])
        );
        assert_eq!(
            g.array("=TAKE(A1:A5, -2)"),
            ArrayValue::new(2, 1, vec![num(4.0), num(5.0)])
        );
        assert_eq!(
            g.array("=DROP(A1:A5, 2)"),
            ArrayValue::new(3, 1, vec![num(3.0), num(4.0), num(5.0)])
        );
        assert_eq!(
            g.array("=DROP(A1:A5, -1)"),
            ArrayValue::new(4, 1, vec![num(1.0), num(2.0), num(3.0), num(4.0)])
        );
        // 2-D take keeps the block
        let g2 = Grid::empty()
            .row("A1", &[1.0, 2.0, 3.0])
            .row("A2", &[4.0, 5.0, 6.0])
            .row("A3", &[7.0, 8.0, 9.0]);
        assert_eq!(
            g2.array("=TAKE(A1:C3, 2)"),
            ArrayValue::new(
                2,
                3,
                vec![num(1.0), num(2.0), num(3.0), num(4.0), num(5.0), num(6.0)]
            )
        );
    }

    #[test]
    fn take_and_drop_zero_and_empty() {
        let g = Grid::empty().col("A1", &[1.0, 2.0, 3.0]);
        assert_eq!(g.error("=TAKE(A1:A3, 0)"), CalcError::Calc);
        assert_eq!(g.error("=DROP(A1:A3, 0)"), CalcError::Value);
        assert_eq!(g.error("=DROP(A1:A3, 3)"), CalcError::Calc);
        assert_eq!(g.error("=TAKE(A1:A3, 3, 0)"), CalcError::Calc);
        // a negative count larger than the array clamps to the whole array
        assert_eq!(
            g.array("=TAKE(A1:A3, -10)"),
            ArrayValue::new(3, 1, vec![num(1.0), num(2.0), num(3.0)])
        );
    }

    // -- CHOOSECOLS / CHOOSEROWS ----------------------------------------------

    #[test]
    fn choosecols_and_chooserows() {
        let g = Grid::empty()
            .row("A1", &[1.0, 2.0, 3.0])
            .row("A2", &[4.0, 5.0, 6.0]);
        assert_eq!(
            g.array("=CHOOSECOLS(A1:C2, 3)"),
            ArrayValue::new(2, 1, vec![num(3.0), num(6.0)])
        );
        assert_eq!(
            g.array("=CHOOSECOLS(A1:C2, 3, 1)"),
            ArrayValue::new(2, 2, vec![num(3.0), num(1.0), num(6.0), num(4.0)])
        );
        assert_eq!(
            g.array("=CHOOSECOLS(A1:C2, -1)"),
            ArrayValue::new(2, 1, vec![num(3.0), num(6.0)])
        );
        assert_eq!(
            g.array("=CHOOSEROWS(A1:C2, 2)"),
            ArrayValue::new(1, 3, vec![num(4.0), num(5.0), num(6.0)])
        );
        assert_eq!(
            g.array("=CHOOSEROWS(A1:C2, 1, 1)"),
            ArrayValue::new(
                2,
                3,
                vec![num(1.0), num(2.0), num(3.0), num(1.0), num(2.0), num(3.0)]
            )
        );
    }

    #[test]
    fn choosecols_zero_or_out_of_range_is_value() {
        let g = Grid::empty().row("A1", &[1.0, 2.0, 3.0]);
        assert_eq!(g.error("=CHOOSECOLS(A1:C1, 0)"), CalcError::Value);
        assert_eq!(g.error("=CHOOSECOLS(A1:C1, 4)"), CalcError::Value);
        assert_eq!(g.error("=CHOOSEROWS(A1:C1, -2)"), CalcError::Value);
    }

    // -- EXPAND ---------------------------------------------------------------

    #[test]
    fn expand_pads_and_errors_on_shrink() {
        let g = Grid::empty().row("A1", &[1.0, 2.0]);
        let a = g.array("=EXPAND(A1:B1, 3, 3)");
        assert_eq!(a.shape(), (3, 3));
        assert_eq!(a.get(0, 0), &num(1.0));
        assert_eq!(a.get(0, 1), &num(2.0));
        assert_eq!(a.get(2, 2), &na());
        let p = g.array("=EXPAND(A1:B1, 2, 3, \"-\")");
        assert_eq!(p.get(1, 2), &txt("-"));
        assert_eq!(g.error("=EXPAND(A1:B1, 1, 1)"), CalcError::Value);
        assert_eq!(g.error("=EXPAND(A1:B1, 0, 2)"), CalcError::Value);
    }

    // -- HSTACK / VSTACK ------------------------------------------------------

    #[test]
    fn hstack_and_vstack_pad_with_na() {
        let g = Grid::empty()
            .row("A1", &[1.0, 2.0])
            .row("A2", &[3.0, 4.0])
            .row("A3", &[5.0, 6.0])
            .set_num("C1", 7.0)
            .set_num("C2", 8.0);
        // HSTACK(A1:B3, C1:C2): the second block has only 2 rows, pad row 3
        let h = g.array("=HSTACK(A1:B3, C1:C2)");
        assert_eq!(h.shape(), (3, 3));
        assert_eq!(h.get(0, 2), &num(7.0));
        assert_eq!(h.get(2, 2), &na());
        // VSTACK(A1:B1, A2:B3): rows stack up
        assert_eq!(
            g.array("=VSTACK(A1:B1, A2:B3)"),
            ArrayValue::new(
                3,
                2,
                vec![num(1.0), num(2.0), num(3.0), num(4.0), num(5.0), num(6.0)]
            )
        );
        // ragged VSTACK: the narrower block is padded with #N/A
        let v = g.array("=VSTACK(A1:B2, A3)");
        assert_eq!(v.shape(), (3, 2));
        assert_eq!(v.get(2, 1), &na());
    }

    // -- TEXTSPLIT ------------------------------------------------------------

    #[test]
    fn textsplit_columns_rows_and_ragged_rows() {
        assert_eq!(
            Grid::empty().array("=TEXTSPLIT(\"a,b,c\", \",\")"),
            ArrayValue::new(1, 3, vec![txt("a"), txt("b"), txt("c")])
        );
        assert_eq!(
            Grid::empty().array("=TEXTSPLIT(\"a,b;c,d\", \",\", \";\")"),
            ArrayValue::new(2, 2, vec![txt("a"), txt("b"), txt("c"), txt("d")])
        );
        let ragged = Grid::empty().array("=TEXTSPLIT(\"a,b;c\", \",\", \";\")");
        assert_eq!(ragged.shape(), (2, 2));
        assert_eq!(ragged.get(1, 1), &na());
        let padded = Grid::empty().array("=TEXTSPLIT(\"a,b;c\", \",\", \";\", , \"-\")");
        assert_eq!(padded.get(1, 1), &txt("-"));
    }

    #[test]
    fn textsplit_ignore_empty_and_row_only() {
        assert_eq!(
            Grid::empty().array("=TEXTSPLIT(\"a,,c\", \",\")"),
            ArrayValue::new(1, 3, vec![txt("a"), txt(""), txt("c")])
        );
        assert_eq!(
            Grid::empty().array("=TEXTSPLIT(\"a,,c\", \",\", , TRUE)"),
            ArrayValue::new(1, 2, vec![txt("a"), txt("c")])
        );
        assert_eq!(
            Grid::empty().array("=TEXTSPLIT(\"a;b\", , \";\")"),
            ArrayValue::new(2, 1, vec![txt("a"), txt("b")])
        );
    }

    #[test]
    fn textsplit_empty_delimiter_is_value() {
        assert_eq!(
            Grid::empty().error("=TEXTSPLIT(\"a,b\", \"\")"),
            CalcError::Value
        );
        assert_eq!(
            Grid::empty().error("=TEXTSPLIT(\"abc\", \"\")"),
            CalcError::Value
        );
    }
}
