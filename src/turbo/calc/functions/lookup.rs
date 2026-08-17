// functions/lookup.rs — the lookup/reference function family. Owned
// exclusively by the lookup family agent; no other agent edits this file.
//
// Registry contract: implement `register` below and keep this exact signature.
// Do NOT edit functions/mod.rs — the `mod lookup;` declaration and the
// `lookup::register(&mut r)` call site in `build()` are already final.
// See functions/mod.rs for the worked ABS template. Note: the lookup family
// is the primary consumer of `FuncArg::as_reference()` and `ctx.cell()`.
use super::{FuncArg, FuncCtx, FuncSpec, MAX_COLS, MAX_ROWS, Registry};
use crate::turbo::calc::ast::{CellRef, RefCore, RefExpr};
use crate::turbo::calc::coerce::{coerce_number, coerce_text, compare, compare_eq};
use crate::turbo::calc::refs::{RefParse, cell_to_a1, parse_a1};
use crate::turbo::calc::value::{ArrayValue, CalcError, CalcValue};
use std::cmp::Ordering;

/// A scalar cell value sanitized against NaN/Inf (the grid invariant keeps
/// numbers finite, but a non-finite cell must surface as #NUM!, never a value).
fn ok_calc(v: CalcValue) -> Result<CalcValue, CalcError> {
    match v {
        CalcValue::Number(n) if !n.is_finite() => Err(CalcError::Num),
        other => Ok(other),
    }
}

/// Wrap any value as a dense array; scalars become 1x1.
fn to_array(v: CalcValue) -> ArrayValue {
    match v {
        CalcValue::Array(a) => (*a).clone(),
        other => ArrayValue::new(1, 1, vec![other]),
    }
}

/// Flatten a one-dimensional array (row or column vector) into a `Vec`.
/// Two-dimensional arrays are a #VALUE! error, as in Excel.
fn as_vec(a: &ArrayValue) -> Result<Vec<CalcValue>, CalcError> {
    if a.rows == 1 || a.cols == 1 {
        Ok(a.data.to_vec())
    } else {
        Err(CalcError::Value)
    }
}

/// Excel's TRUE/FALSE argument coercion (VLOOKUP/HLOOKUP `range_lookup`):
/// booleans, numbers (non-zero = TRUE), and the literal strings
/// "TRUE"/"FALSE". Blank defaults to TRUE.
fn as_bool_arg(v: &CalcValue) -> Result<bool, CalcError> {
    match v {
        CalcValue::Bool(b) => Ok(*b),
        CalcValue::Number(n) => Ok(*n != 0.0),
        CalcValue::Blank => Ok(true),
        CalcValue::Text(t) => match t.trim().to_ascii_uppercase().as_str() {
            "TRUE" => Ok(true),
            "FALSE" => Ok(false),
            _ => Ok(coerce_number(v)? != 0.0),
        },
        CalcValue::Error(e) => Err(*e),
        CalcValue::Array(_) => Err(CalcError::Value),
    }
}

/// First exact-match position (or the last when `keep_last`). When
/// `wildcards` is set the needle is applied as a `*`/`?`/`~` pattern.
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

/// Approximate scan over the global Excel ordering (compare from coerce.rs):
/// with `larger` false, the largest value <= `needle`; with `larger` true, the
/// smallest value >= `needle`. Ties keep the first position unless
/// `keep_last` (used by the "last to first" search directions and MATCH's
/// "largest position" rule).
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

/// VLOOKUP(lookup, table, col_index, [range_lookup]).
fn vlookup(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let needle = args[0].value(ctx)?;
    let table = to_array(args[1].value(ctx)?);
    let ci = coerce_number(&args[2].value(ctx)?)?.trunc() as i64;
    let approximate = if args.len() > 3 {
        as_bool_arg(&args[3].value(ctx)?)?
    } else {
        true
    };
    if ci < 1 {
        return Err(CalcError::Value);
    }
    if ci > table.cols as i64 {
        return Err(CalcError::Ref);
    }
    let first_col: Vec<CalcValue> = (0..table.rows).map(|r| table.get(r, 0).clone()).collect();
    let pos = if approximate {
        best_scan(&first_col, &needle, false, true)?
    } else {
        exact_scan(&first_col, &needle, false, false)?
    };
    match pos {
        Some(r) => ok_calc(table.get(r as u32, (ci - 1) as u32).clone()),
        None => Err(CalcError::Na),
    }
}

/// HLOOKUP(lookup, table, row_index, [range_lookup]).
fn hlookup(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let needle = args[0].value(ctx)?;
    let table = to_array(args[1].value(ctx)?);
    let ri = coerce_number(&args[2].value(ctx)?)?.trunc() as i64;
    let approximate = if args.len() > 3 {
        as_bool_arg(&args[3].value(ctx)?)?
    } else {
        true
    };
    if ri < 1 {
        return Err(CalcError::Value);
    }
    if ri > table.rows as i64 {
        return Err(CalcError::Ref);
    }
    let first_row: Vec<CalcValue> = (0..table.cols).map(|c| table.get(0, c).clone()).collect();
    let pos = if approximate {
        best_scan(&first_row, &needle, false, true)?
    } else {
        exact_scan(&first_row, &needle, false, false)?
    };
    match pos {
        Some(c) => ok_calc(table.get((ri - 1) as u32, c as u32).clone()),
        None => Err(CalcError::Na),
    }
}

/// XLOOKUP(lookup, lookup_array, return_array, [if_not_found], [match_mode],
/// [search_mode]).
// XLOOKUP is registered by `dynamic.rs`, which owns the dynamic-array
// families. This copy stays as an independent cross-check: its unit tests
// below exercise the same semantics against a second implementation.
#[allow(dead_code)]
fn xlookup(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let needle = args[0].value(ctx)?;
    let look = to_array(args[1].value(ctx)?);
    let ret = to_array(args[2].value(ctx)?);
    let look_vals = as_vec(&look)?;
    let ret_vals = as_vec(&ret)?;
    if look_vals.len() != ret_vals.len() {
        return Err(CalcError::Value);
    }
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
    if !matches!(search_mode, 1 | -1) {
        return Err(CalcError::Value);
    }
    let keep_last = search_mode == -1;
    let pos = match match_mode {
        0 => exact_scan(&look_vals, &needle, false, keep_last)?,
        1 => best_scan(&look_vals, &needle, true, keep_last)?,
        -1 => best_scan(&look_vals, &needle, false, keep_last)?,
        2 => exact_scan(&look_vals, &needle, true, keep_last)?,
        _ => return Err(CalcError::Value),
    };
    match pos {
        Some(i) => ok_calc(ret_vals[i].clone()),
        None => match args.get(3) {
            Some(a) => ok_calc(a.value(ctx)?),
            None => Err(CalcError::Na),
        },
    }
}

/// MATCH(lookup, array, [match_type]).
fn match_(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let needle = args[0].value(ctx)?;
    let vals = as_vec(&to_array(args[1].value(ctx)?))?;
    let mt = if args.len() > 2 {
        let v = args[2].value(ctx)?;
        if v.is_blank() {
            1
        } else {
            coerce_number(&v)? as i64
        }
    } else {
        1
    };
    let pos = match mt {
        1 => best_scan(&vals, &needle, false, true)?,
        -1 => best_scan(&vals, &needle, true, false)?,
        0 => exact_scan(&vals, &needle, matches!(&needle, CalcValue::Text(_)), false)?,
        _ => return Err(CalcError::Value),
    };
    match pos {
        Some(i) => Ok(CalcValue::number((i + 1) as f64)),
        None => Err(CalcError::Na),
    }
}

/// INDEX(array, row_num, [col_num]).
fn index(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let a = to_array(args[0].value(ctx)?);
    let rn = coerce_number(&args[1].value(ctx)?)?.trunc() as i64;
    let has_col = args.len() > 2;
    let cn = if has_col {
        coerce_number(&args[2].value(ctx)?)?.trunc() as i64
    } else {
        0
    };
    if rn == 0 && cn == 0 {
        return Ok(CalcValue::array(a.clone()));
    }
    if rn < 0 || cn < 0 {
        return Err(CalcError::Value);
    }
    if has_col {
        if rn == 0 {
            if cn == 0 || cn > a.cols as i64 {
                return Err(CalcError::Ref);
            }
            let col: Vec<CalcValue> = (0..a.rows)
                .map(|r| a.get(r, (cn - 1) as u32).clone())
                .collect();
            return Ok(CalcValue::array(ArrayValue::new(a.rows, 1, col)));
        }
        if cn == 0 {
            if rn > a.rows as i64 {
                return Err(CalcError::Ref);
            }
            let row: Vec<CalcValue> = (0..a.cols)
                .map(|c| a.get((rn - 1) as u32, c).clone())
                .collect();
            return Ok(CalcValue::array(ArrayValue::new(1, a.cols, row)));
        }
        if rn > a.rows as i64 || cn > a.cols as i64 {
            return Err(CalcError::Ref);
        }
        return ok_calc(a.get((rn - 1) as u32, (cn - 1) as u32).clone());
    }
    // col_num omitted: single-column arrays return the cell; wider arrays
    // return the whole row.
    if rn == 0 {
        return Ok(CalcValue::array(a.clone()));
    }
    if rn > a.rows as i64 {
        return Err(CalcError::Ref);
    }
    if a.cols == 1 {
        return ok_calc(a.get((rn - 1) as u32, 0).clone());
    }
    let row: Vec<CalcValue> = (0..a.cols)
        .map(|c| a.get((rn - 1) as u32, c).clone())
        .collect();
    Ok(CalcValue::array(ArrayValue::new(1, a.cols, row)))
}

/// LOOKUP(value, lookup_vector, [result_vector]) / LOOKUP(value, array).
fn lookup(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let needle = args[0].value(ctx)?;
    if args.len() == 2 {
        // Array form: wider than tall searches the first row and returns the
        // matched column's last cell; otherwise the first column is searched
        // and the matched row's last cell is returned.
        let a = to_array(args[1].value(ctx)?);
        let by_row = a.cols > a.rows;
        let values: Vec<CalcValue> = if by_row {
            (0..a.cols).map(|c| a.get(0, c).clone()).collect()
        } else {
            (0..a.rows).map(|r| a.get(r, 0).clone()).collect()
        };
        let pos = best_scan(&values, &needle, false, true)?;
        return match pos {
            Some(i) if by_row => ok_calc(a.get(a.rows - 1, i as u32).clone()),
            Some(i) => ok_calc(a.get(i as u32, a.cols - 1).clone()),
            None => Err(CalcError::Na),
        };
    }
    let look = to_array(args[1].value(ctx)?);
    let res = to_array(args[2].value(ctx)?);
    let look_vals = as_vec(&look)?;
    let res_vals = as_vec(&res)?;
    if look_vals.len() != res_vals.len() {
        return Err(CalcError::Value);
    }
    match best_scan(&look_vals, &needle, false, true)? {
        Some(i) => ok_calc(res_vals[i].clone()),
        None => Err(CalcError::Na),
    }
}

/// CHOOSE(index, v1, v2, ...).
fn choose(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?.trunc() as i64;
    if n < 1 || n as usize > args.len() - 1 {
        return Err(CalcError::Value);
    }
    ok_calc(args[n as usize].value(ctx)?)
}

/// Largest materialized OFFSET result we will build (mirrors the eval-loop cap);
/// anything bigger is `#VALUE!` rather than a risky allocation.
const MAX_OFFSET_CELLS: usize = 4_000_000;

/// The base of an OFFSET call: the sheet to read and the top-left cell plus the
/// default height/width (the base reference's own extent).
type BaseRef = (u32, i64, i64, i64, i64);

/// OFFSET(reference, rows, cols, [height], [width]).
///
/// VALUE FORM ONLY. OFFSET returns the shifted region as a materialised value —
/// a single cell, or a dense array — which makes `SUM(OFFSET(...))` and any
/// other value consumer work. It cannot produce a live reference: the engine has
/// no reference value type, so coordinate-consuming functions such as
/// `ROW(OFFSET(...))` cannot be fed by it. That limitation is stated here rather
/// than faked.
fn offset(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let dr = coerce_number(&args[1].value(ctx)?)?.trunc() as i64;
    let dc = coerce_number(&args[2].value(ctx)?)?.trunc() as i64;
    let (sheet, r0, c0, h0, w0): BaseRef = match &args[0] {
        FuncArg::Value(CalcValue::Error(e)) => return Err(*e),
        FuncArg::Reference(re) => {
            let (sheet, core) = match re {
                RefExpr::Local(core) => (ctx.sheet, core),
                RefExpr::Sheet { name, inner } => {
                    let s = ctx.resolver.sheet_index(name).ok_or(CalcError::Ref)?;
                    (s, inner.as_ref())
                }
                _ => return Err(CalcError::Ref),
            };
            match core {
                RefCore::Cell(c) => (sheet, i64::from(c.row), i64::from(c.col), 1, 1),
                RefCore::Range(r) => (
                    sheet,
                    i64::from(r.start.row),
                    i64::from(r.start.col),
                    i64::from(r.end.row) - i64::from(r.start.row) + 1,
                    i64::from(r.end.col) - i64::from(r.start.col) + 1,
                ),
                RefCore::Row(r) => (
                    sheet,
                    i64::from(r.start),
                    0,
                    i64::from(r.end) - i64::from(r.start) + 1,
                    i64::from(MAX_COLS),
                ),
                RefCore::Column(c) => (
                    sheet,
                    0,
                    i64::from(c.start),
                    i64::from(MAX_ROWS),
                    i64::from(c.end) - i64::from(c.start) + 1,
                ),
            }
        }
        // A non-reference first argument is #VALUE! in Excel too.
        FuncArg::Value(_) | FuncArg::Expr(_) => return Err(CalcError::Value),
    };
    let rows = r0 + dr;
    let cols = c0 + dc;
    if rows < 0 || cols < 0 || rows >= i64::from(MAX_ROWS) || cols >= i64::from(MAX_COLS) {
        return Err(CalcError::Ref);
    }
    let h = offset_dim(ctx, args.get(3), h0)?;
    let w = offset_dim(ctx, args.get(4), w0)?;
    // Zero or negative height/width is #REF! (Excel), never a degenerate range.
    if h <= 0 || w <= 0 {
        return Err(CalcError::Ref);
    }
    if rows + h > i64::from(MAX_ROWS) || cols + w > i64::from(MAX_COLS) {
        return Err(CalcError::Ref);
    }
    if h == 1 && w == 1 {
        return Ok(ctx
            .resolver
            .cell(sheet, rows as u32, cols as u32)
            .unwrap_or(CalcValue::Blank));
    }
    let n = (h * w) as usize;
    if n > MAX_OFFSET_CELLS {
        return Err(CalcError::Value);
    }
    let mut data = Vec::with_capacity(n);
    for r in 0..h {
        for c in 0..w {
            data.push(
                ctx.resolver
                    .cell(sheet, (rows + r) as u32, (cols + c) as u32)
                    .unwrap_or(CalcValue::Blank),
            );
        }
    }
    Ok(CalcValue::array(ArrayValue::new(h as u32, w as u32, data)))
}

/// One of OFFSET's optional height/width arguments: omitted or explicitly blank
/// means the base reference's own extent (Excel treats an empty argument as
/// omitted).
fn offset_dim(ctx: &FuncCtx, arg: Option<&FuncArg>, default: i64) -> Result<i64, CalcError> {
    match arg {
        None => Ok(default),
        Some(a) => {
            let v = a.value(ctx)?;
            if v.is_blank() {
                Ok(default)
            } else {
                Ok(coerce_number(&v)?.trunc() as i64)
            }
        }
    }
}

/// INDIRECT(ref_text, [a1]).
///
/// VALUE FORM ONLY. Parses `ref_text` at evaluation time and materialises the
/// reference — single cell to a value, range to a dense array. A live reference
/// cannot be produced (no reference value type), so `ROW(INDIRECT(...))` is out
/// of reach; an unparseable or out-of-grid string is `#REF!`.
fn indirect(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let text = coerce_text(&args[0].value(ctx)?)?;
    let a1 = if args.len() > 1 {
        as_bool_arg(&args[1].value(ctx)?)?
    } else {
        true
    };
    let re = if a1 {
        parse_indirect_a1(&text)?
    } else {
        parse_indirect_r1c1(&text, ctx.row, ctx.col)?
    };
    ctx.resolve(&re)
}

/// Parse `ref_text` as an A1 reference (cell, range, whole row/column), with an
/// optional `Sheet!` qualifier. Anything unparseable or out of grid is `#REF!`.
fn parse_indirect_a1(text: &str) -> Result<RefExpr, CalcError> {
    let t = text.trim();
    let (sheet, core) = match t.rfind('!') {
        Some(pos) => (Some(unquote_sheet(&t[..pos])), t[pos + 1..].trim()),
        None => (None, t),
    };
    let rc = match parse_a1(core) {
        RefParse::Ref(rc) => rc,
        _ => return Err(CalcError::Ref),
    };
    Ok(match sheet {
        Some(name) => RefExpr::Sheet {
            name,
            inner: Box::new(rc),
        },
        None => RefExpr::Local(rc),
    })
}

/// Parse `ref_text` as an R1C1 cell (absolute `R5C3`, relative `R[-1]C[2]`, or
/// `RC` for the formula cell itself), anchored at the formula cell. An optional
/// `Sheet!` qualifier is honoured. Anything else is `#REF!` — FALSE style is
/// never silently reinterpreted as A1.
fn parse_indirect_r1c1(text: &str, base_row: u32, base_col: u32) -> Result<RefExpr, CalcError> {
    let t = text.trim();
    let (sheet, core) = match t.rfind('!') {
        Some(pos) => (Some(unquote_sheet(&t[..pos])), t[pos + 1..].trim()),
        None => (None, t),
    };
    let c = parse_r1c1_cell(core, base_row, base_col)?;
    Ok(match sheet {
        Some(name) => RefExpr::Sheet {
            name,
            inner: Box::new(RefCore::Cell(c)),
        },
        None => RefExpr::Local(RefCore::Cell(c)),
    })
}

/// One `R[n]C[m]` cell, 0-based. Bounds are checked against the Excel grid.
fn parse_r1c1_cell(s: &str, base_row: u32, base_col: u32) -> Result<CellRef, CalcError> {
    let b = s.as_bytes();
    let mut i = 0;
    if i >= b.len() || !(b[i] == b'R' || b[i] == b'r') {
        return Err(CalcError::Ref);
    }
    i += 1;
    let row = r1c1_part(b, s, &mut i, base_row)?;
    if i >= b.len() || !(b[i] == b'C' || b[i] == b'c') {
        return Err(CalcError::Ref);
    }
    i += 1;
    let col = r1c1_part(b, s, &mut i, base_col)?;
    if i != b.len() {
        return Err(CalcError::Ref);
    }
    if row < 1 || row > i64::from(MAX_ROWS) || col < 1 || col > i64::from(MAX_COLS) {
        return Err(CalcError::Ref);
    }
    Ok(CellRef {
        col: (col - 1) as u16,
        row: (row - 1) as u32,
        abs_col: false,
        abs_row: false,
    })
}

/// One `R`/`C` value: nothing -> the current position, `[n]` -> relative (can be
/// negative), plain digits -> absolute. Returns a 1-based coordinate. The
/// "nothing" case covers both end-of-string (`R1C`) and the start of the next
/// piece (`RC`, where the `R` value ends at the `C`).
fn r1c1_part(b: &[u8], s: &str, i: &mut usize, base: u32) -> Result<i64, CalcError> {
    if *i >= b.len() {
        return Ok(i64::from(base) + 1);
    }
    if b[*i] == b'[' {
        *i += 1;
        let neg = b.get(*i) == Some(&b'-');
        if neg {
            *i += 1;
        }
        let start = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        if *i == start || *i >= b.len() || b[*i] != b']' {
            return Err(CalcError::Ref);
        }
        let n: i64 = s[start..*i].parse().map_err(|_| CalcError::Ref)?;
        *i += 1;
        let delta = if neg { -n } else { n };
        Ok(i64::from(base) + 1 + delta)
    } else if b[*i].is_ascii_digit() {
        let start = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        s[start..*i].parse::<i64>().map_err(|_| CalcError::Ref)
    } else {
        // No value (e.g. `RC`): the current position, leaving the next piece
        // for the caller to consume.
        Ok(i64::from(base) + 1)
    }
}

/// Undo the single-quote quoting of a `Sheet!` qualifier, doubling removed.
fn unquote_sheet(s: &str) -> String {
    let t = s.trim();
    if t.len() >= 2 && t.starts_with('\'') && t.ends_with('\'') {
        t[1..t.len() - 1].replace("''", "'")
    } else {
        t.to_string()
    }
}

/// ADDRESS(row, col, [abs_num], [a1], [sheet]).
///
/// Builds a reference STRING. `abs_num` 1-4 selects the `$` pattern (1 = $A$1,
/// 2 = A$1, 3 = $A1, 4 = A1); with `a1` FALSE it selects the R1C1 spelling
/// instead (1 = R1C1 ... 4 = R[1]C[1]). A sheet name with a space or an
/// apostrophe is quoted, with internal apostrophes doubled.
fn address(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let row = coerce_number(&args[0].value(ctx)?)?.trunc() as i64;
    let col = coerce_number(&args[1].value(ctx)?)?.trunc() as i64;
    let abs_num = if args.len() > 2 {
        coerce_number(&args[2].value(ctx)?)?.trunc() as i64
    } else {
        1
    };
    let a1 = if args.len() > 3 {
        as_bool_arg(&args[3].value(ctx)?)?
    } else {
        true
    };
    let sheet = if args.len() > 4 {
        Some(coerce_text(&args[4].value(ctx)?)?)
    } else {
        None
    };
    if row < 1 || row > i64::from(MAX_ROWS) || col < 1 || col > i64::from(MAX_COLS) {
        return Err(CalcError::Value);
    }
    if !(1..=4).contains(&abs_num) {
        return Err(CalcError::Value);
    }
    let core = if a1 {
        cell_to_a1(&CellRef {
            col: (col - 1) as u16,
            row: (row - 1) as u32,
            abs_col: matches!(abs_num, 1 | 3),
            abs_row: matches!(abs_num, 1 | 2),
        })
    } else {
        let r = if matches!(abs_num, 1 | 2) {
            format!("R{row}")
        } else {
            format!("R[{row}]")
        };
        let c = if matches!(abs_num, 1 | 3) {
            format!("C{col}")
        } else {
            format!("C[{col}]")
        };
        format!("{r}{c}")
    };
    match sheet {
        Some(name) if !name.is_empty() => {
            let prefix = if needs_quote(&name) {
                format!("'{}'", name.replace('\'', "''"))
            } else {
                name
            };
            Ok(CalcValue::text(format!("{prefix}!{core}")))
        }
        _ => Ok(CalcValue::text(core)),
    }
}

/// Excel quotes a sheet name in a reference when it contains anything outside
/// `[A-Za-z0-9_.]` — in particular a space or an apostrophe.
fn needs_quote(name: &str) -> bool {
    name.chars()
        .any(|c| c == ' ' || c == '\'' || !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
}

/// HYPERLINK(link, [friendly_name]). The computed value is the friendly name
/// (the second argument), or the link itself when omitted; the link is
/// presentation, not part of the value.
fn hyperlink(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    if args.len() > 1 {
        args[1].value(ctx)
    } else {
        args[0].value(ctx)
    }
}

// -- AREAS / FORMULATEXT / IMAGE / WRAPCOLS / WRAPROWS -----------------------

/// `AREAS(reference)`: the number of areas in a reference. The engine has no
/// union-reference value type, so every reference it can hold is exactly one
/// area and the answer is `1`; a non-reference argument is `#VALUE!`.
fn areas(_ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    if args[0].as_reference().is_some() {
        Ok(CalcValue::Number(1.0))
    } else {
        Err(CalcError::Value)
    }
}

/// `FORMULATEXT(reference)`: the formula text of the referenced cell, or `#N/A`
/// when the cell is not a formula.
///
/// The engine's `CellResolver` exposes computed values only, never the formula
/// string, so no cell can be reported as holding one — the answer is `#N/A`,
/// matching Excel's non-formula case. A non-reference argument is `#VALUE!`, as
/// in Excel. Workbooks that need the real formula text route through the
/// fallback path (`fullCalcOnLoad="1"`).
fn formulatext(_ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    if args[0].as_reference().is_some() {
        Err(CalcError::Na)
    } else {
        Err(CalcError::Value)
    }
}

/// `IMAGE(url, [alt], [sizing], [height], [width])`.
///
/// STUB: a calculation engine returns values, not rendered images, so every
/// call is `#VALUE!`. The function exists and is registered so workbooks
/// calling it parse and route to the fallback path instead of failing as an
/// unknown name.
fn image(_ctx: &FuncCtx, _args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    Err(CalcError::Value)
}

/// Cell cap for a WRAPCOLS/WRAPROWS result (~4M cells, the same budget as
/// OFFSET's materialization).
const MAX_WRAP_CELLS: usize = 4_000_000;

/// The elements of a one-dimensional array (row or column vector), in scan
/// order. A 2-D array is `#VALUE!`, matching Excel; an empty array is a valid
/// empty vector (its caller decides the empty-result error).
fn wrap_vector(a: &ArrayValue) -> Result<Vec<CalcValue>, CalcError> {
    if a.rows == 0 || a.cols == 0 {
        return Ok(Vec::new());
    }
    if a.rows == 1 {
        Ok((0..a.cols).map(|c| a.get(0, c).clone()).collect())
    } else if a.cols == 1 {
        Ok((0..a.rows).map(|r| a.get(r, 0).clone()).collect())
    } else {
        Err(CalcError::Value)
    }
}

/// `WRAPROWS(vector, wrap_count, [pad_with])`: the vector laid out row by row,
/// `wrap_count` cells per row, padded to a full rectangle with `pad_with`
/// (default `#N/A`). Element `i` of the vector lands at `(i / wrap_count,
/// i % wrap_count)`.
fn wraprows(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    wrap_impl(ctx, args, false)
}

/// `WRAPCOLS(vector, wrap_count, [pad_with])`: the vector laid out column by
/// column, `wrap_count` cells per column. Element `i` lands at
/// `(i % wrap_count, i / wrap_count)`.
fn wrapcols(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    wrap_impl(ctx, args, true)
}

fn wrap_impl(ctx: &FuncCtx, args: &[FuncArg], by_col: bool) -> Result<CalcValue, CalcError> {
    let array = to_array(args[0].value(ctx)?);
    let values = wrap_vector(&array)?;
    let wrap = coerce_number(&args[1].value(ctx)?)?.trunc() as i64;
    let pad = if args.len() > 2 {
        args[2].value(ctx)?
    } else {
        CalcValue::err(CalcError::Na)
    };
    if wrap < 1 {
        return Err(CalcError::Value);
    }
    if values.is_empty() {
        return Err(CalcError::Calc);
    }
    let n = values.len();
    let wrap = wrap as usize;
    let groups = n.div_ceil(wrap);
    let (rows, cols) = if by_col {
        (wrap as u32, groups as u32)
    } else {
        (groups as u32, wrap as u32)
    };
    let total = (rows as usize) * (cols as usize);
    if total > MAX_WRAP_CELLS {
        return Err(CalcError::Value);
    }
    let mut data = vec![pad; total];
    for (i, v) in values.iter().enumerate() {
        let (r, c) = if by_col {
            (i % wrap, i / wrap)
        } else {
            (i / wrap, i % wrap)
        };
        data[r * cols as usize + c] = v.clone();
    }
    Ok(CalcValue::array(ArrayValue::new(rows, cols, data)))
}

const VLOOKUP: FuncSpec = FuncSpec {
    name: "VLOOKUP",
    min_args: 3,
    max_args: Some(4),
    volatile: false,
    array_aware: true,
    func: vlookup,
};

const HLOOKUP: FuncSpec = FuncSpec {
    name: "HLOOKUP",
    min_args: 3,
    max_args: Some(4),
    volatile: false,
    array_aware: true,
    func: hlookup,
};

#[allow(dead_code)]
const XLOOKUP: FuncSpec = FuncSpec {
    name: "XLOOKUP",
    min_args: 3,
    max_args: Some(6),
    volatile: false,
    array_aware: true,
    func: xlookup,
};

const INDEX: FuncSpec = FuncSpec {
    name: "INDEX",
    min_args: 2,
    max_args: Some(3),
    volatile: false,
    array_aware: true,
    func: index,
};

const MATCH: FuncSpec = FuncSpec {
    name: "MATCH",
    min_args: 2,
    max_args: Some(3),
    volatile: false,
    array_aware: true,
    func: match_,
};

const LOOKUP: FuncSpec = FuncSpec {
    name: "LOOKUP",
    min_args: 2,
    max_args: Some(3),
    volatile: false,
    array_aware: true,
    func: lookup,
};

const CHOOSE: FuncSpec = FuncSpec {
    name: "CHOOSE",
    min_args: 2,
    max_args: None,
    volatile: false,
    array_aware: true,
    func: choose,
};

const OFFSET: FuncSpec = FuncSpec {
    name: "OFFSET",
    min_args: 3,
    max_args: Some(5),
    volatile: true,
    array_aware: false,
    func: offset,
};

const INDIRECT: FuncSpec = FuncSpec {
    name: "INDIRECT",
    min_args: 1,
    max_args: Some(2),
    volatile: true,
    array_aware: false,
    func: indirect,
};

const ADDRESS: FuncSpec = FuncSpec {
    name: "ADDRESS",
    min_args: 2,
    max_args: Some(5),
    volatile: false,
    array_aware: false,
    func: address,
};

const HYPERLINK: FuncSpec = FuncSpec {
    name: "HYPERLINK",
    min_args: 1,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: hyperlink,
};

const AREAS: FuncSpec = FuncSpec {
    name: "AREAS",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: true,
    func: areas,
};

const FORMULATEXT: FuncSpec = FuncSpec {
    name: "FORMULATEXT",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: formulatext,
};

const IMAGE: FuncSpec = FuncSpec {
    name: "IMAGE",
    min_args: 1,
    max_args: Some(5),
    volatile: false,
    array_aware: false,
    func: image,
};

const WRAPROWS: FuncSpec = FuncSpec {
    name: "WRAPROWS",
    min_args: 2,
    max_args: Some(3),
    volatile: false,
    array_aware: true,
    func: wraprows,
};

const WRAPCOLS: FuncSpec = FuncSpec {
    name: "WRAPCOLS",
    min_args: 2,
    max_args: Some(3),
    volatile: false,
    array_aware: true,
    func: wrapcols,
};

pub fn register(r: &mut Registry) {
    r.register(&VLOOKUP);
    r.register(&HLOOKUP);
    // XLOOKUP is NOT registered here. The registered implementation lives in
    // `functions/dynamic.rs`, because XLOOKUP is a dynamic-array function: its
    // `return_array` may be wider than one column, in which case it returns a
    // block that spills. This file's version only ever returns a single value,
    // so registering it would cap XLOOKUP at the pre-2020 behaviour — and
    // `Registry::register` debug_asserts on a duplicate name, so only one may
    // be registered at a time.
    //
    // The `XLOOKUP` const below and its unit tests are deliberately KEPT as an
    // independent cross-check: two implementations written from the same spec
    // that agree on the scalar cases are better evidence than one. The
    // registry-driven tests in `tests_class_logical_lookup.rs` exercise the
    // dynamic.rs version through the real parse-then-eval path.
    r.register(&INDEX);
    r.register(&MATCH);
    r.register(&LOOKUP);
    r.register(&CHOOSE);
    r.register(&OFFSET);
    r.register(&INDIRECT);
    r.register(&ADDRESS);
    r.register(&HYPERLINK);
    r.register(&AREAS);
    r.register(&FORMULATEXT);
    r.register(&IMAGE);
    r.register(&WRAPROWS);
    r.register(&WRAPCOLS);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbo::calc::ast::{CellRef, RangeRef, RefCore, RefExpr};
    use crate::turbo::calc::functions::CellResolver;
    use std::collections::HashMap;

    struct GridResolver {
        grid: HashMap<(u32, u32), CalcValue>,
    }

    impl GridResolver {
        fn new() -> Self {
            GridResolver {
                grid: HashMap::new(),
            }
        }
        fn put(&mut self, row: u32, col: u32, v: CalcValue) {
            self.grid.insert((row, col), v);
        }
    }

    impl CellResolver for GridResolver {
        fn cell(&self, sheet: u32, row: u32, col: u32) -> Option<CalcValue> {
            if sheet == 0 {
                self.grid.get(&(row, col)).cloned()
            } else {
                None
            }
        }
        fn sheet_index(&self, name: &str) -> Option<u32> {
            if name == "Data" { Some(0) } else { None }
        }
    }

    fn ctx(resolver: &dyn CellResolver) -> FuncCtx {
        FuncCtx {
            date1904: false,
            sheet: 0,
            row: 0,
            col: 0,
            resolver,
        }
    }

    fn call(
        spec: &FuncSpec,
        resolver: &dyn CellResolver,
        args: Vec<FuncArg>,
    ) -> Result<CalcValue, CalcError> {
        let c = ctx(resolver);
        (spec.func)(&c, &args)
    }

    fn num(n: f64) -> CalcValue {
        CalcValue::number(n)
    }
    fn txt(s: &str) -> CalcValue {
        CalcValue::text(s)
    }
    fn col(data: Vec<CalcValue>) -> CalcValue {
        CalcValue::array(ArrayValue::new(data.len() as u32, 1, data))
    }
    fn arr(rows: u32, cols: u32, data: Vec<CalcValue>) -> CalcValue {
        CalcValue::array(ArrayValue::new(rows, cols, data))
    }

    fn range_ref(r0: u32, c0: u16, r1: u32, c1: u16) -> RefExpr {
        RefExpr::Local(RefCore::Range(RangeRef {
            start: CellRef {
                col: c0,
                row: r0,
                abs_col: false,
                abs_row: false,
            },
            end: CellRef {
                col: c1,
                row: r1,
                abs_col: false,
                abs_row: false,
            },
        }))
    }

    #[test]
    fn vlookup_exact_and_approx_through_reference() {
        let mut r = GridResolver::new();
        r.put(0, 0, num(1.0));
        r.put(0, 1, txt("a"));
        r.put(1, 0, num(2.0));
        r.put(1, 1, txt("b"));
        r.put(2, 0, num(3.0));
        r.put(2, 1, txt("c"));
        r.put(3, 0, num(5.0));
        r.put(3, 1, txt("e"));
        let tbl = FuncArg::Reference(range_ref(0, 0, 3, 1));
        assert_eq!(
            call(
                &VLOOKUP,
                &r,
                vec![
                    FuncArg::Value(num(2.0)),
                    tbl.clone(),
                    FuncArg::Value(num(2.0)),
                    FuncArg::Value(CalcValue::Bool(false)),
                ]
            ),
            Ok(txt("b"))
        );
        assert_eq!(
            call(
                &VLOOKUP,
                &r,
                vec![
                    FuncArg::Value(num(4.0)),
                    tbl.clone(),
                    FuncArg::Value(num(2.0)),
                ]
            ),
            Ok(txt("c"))
        );
        assert_eq!(
            call(
                &VLOOKUP,
                &r,
                vec![
                    FuncArg::Value(num(5.0)),
                    tbl,
                    FuncArg::Value(num(2.0)),
                    FuncArg::Value(txt("FALSE")),
                ]
            ),
            Ok(txt("e"))
        );
    }

    #[test]
    fn vlookup_errors() {
        let mut r = GridResolver::new();
        r.put(0, 0, num(1.0));
        r.put(0, 1, txt("a"));
        r.put(1, 0, num(2.0));
        r.put(1, 1, txt("b"));
        let tbl = FuncArg::Reference(range_ref(0, 0, 1, 1));
        assert_eq!(
            call(
                &VLOOKUP,
                &r,
                vec![
                    FuncArg::Value(num(1.0)),
                    tbl.clone(),
                    FuncArg::Value(num(0.0)),
                    FuncArg::Value(CalcValue::Bool(false)),
                ]
            ),
            Err(CalcError::Value)
        );
        assert_eq!(
            call(
                &VLOOKUP,
                &r,
                vec![
                    FuncArg::Value(num(1.0)),
                    tbl.clone(),
                    FuncArg::Value(num(3.0)),
                    FuncArg::Value(CalcValue::Bool(false)),
                ]
            ),
            Err(CalcError::Ref)
        );
        assert_eq!(
            call(
                &VLOOKUP,
                &r,
                vec![
                    FuncArg::Value(num(9.0)),
                    tbl.clone(),
                    FuncArg::Value(num(2.0)),
                    FuncArg::Value(CalcValue::Bool(false)),
                ]
            ),
            Err(CalcError::Na)
        );
        assert_eq!(
            call(
                &VLOOKUP,
                &r,
                vec![FuncArg::Value(num(0.0)), tbl, FuncArg::Value(num(2.0)),]
            ),
            Err(CalcError::Na)
        );
    }

    #[test]
    fn vlookup_approximate_uses_excel_cross_type_ordering() {
        let mut r = GridResolver::new();
        r.put(0, 0, txt("z"));
        r.put(0, 1, txt("Z"));
        r.put(1, 0, num(1.0));
        r.put(1, 1, txt("one"));
        r.put(2, 0, CalcValue::Bool(false));
        r.put(2, 1, txt("F"));
        r.put(3, 0, CalcValue::Bool(true));
        r.put(3, 1, txt("T"));
        // Number < text("x") < text("z") < FALSE < TRUE
        assert_eq!(
            call(
                &VLOOKUP,
                &r,
                vec![
                    FuncArg::Value(txt("x")),
                    FuncArg::Reference(range_ref(0, 0, 3, 1)),
                    FuncArg::Value(num(2.0)),
                ]
            ),
            Ok(txt("one"))
        );
    }

    #[test]
    fn hlookup_exact_and_approx() {
        let mut r = GridResolver::new();
        r.put(0, 0, num(1.0));
        r.put(0, 1, num(3.0));
        r.put(0, 2, num(5.0));
        r.put(1, 0, txt("a"));
        r.put(1, 1, txt("c"));
        r.put(1, 2, txt("e"));
        let tbl = FuncArg::Reference(range_ref(0, 0, 1, 2));
        assert_eq!(
            call(
                &HLOOKUP,
                &r,
                vec![
                    FuncArg::Value(num(3.0)),
                    tbl.clone(),
                    FuncArg::Value(num(2.0)),
                    FuncArg::Value(CalcValue::Bool(false)),
                ]
            ),
            Ok(txt("c"))
        );
        assert_eq!(
            call(
                &HLOOKUP,
                &r,
                vec![
                    FuncArg::Value(num(4.0)),
                    tbl.clone(),
                    FuncArg::Value(num(2.0)),
                ]
            ),
            Ok(txt("c"))
        );
        assert_eq!(
            call(
                &HLOOKUP,
                &r,
                vec![
                    FuncArg::Value(num(1.0)),
                    tbl.clone(),
                    FuncArg::Value(num(0.0)),
                ]
            ),
            Err(CalcError::Value)
        );
        assert_eq!(
            call(
                &HLOOKUP,
                &r,
                vec![FuncArg::Value(num(1.0)), tbl, FuncArg::Value(num(4.0)),]
            ),
            Err(CalcError::Ref)
        );
    }

    #[test]
    fn xlookup_exact_and_approximate_modes() {
        let r = GridResolver::new();
        let look = col(vec![num(1.0), num(2.0), num(3.0)]);
        let ret = col(vec![num(10.0), num(20.0), num(30.0)]);
        assert_eq!(
            call(
                &XLOOKUP,
                &r,
                vec![
                    FuncArg::Value(num(2.0)),
                    FuncArg::Value(look.clone()),
                    FuncArg::Value(ret.clone()),
                ]
            ),
            Ok(num(20.0))
        );
        assert_eq!(
            call(
                &XLOOKUP,
                &r,
                vec![
                    FuncArg::Value(num(2.5)),
                    FuncArg::Value(look.clone()),
                    FuncArg::Value(ret.clone()),
                    FuncArg::Value(CalcValue::Blank),
                    FuncArg::Value(num(-1.0)),
                ]
            ),
            Ok(num(20.0))
        );
        assert_eq!(
            call(
                &XLOOKUP,
                &r,
                vec![
                    FuncArg::Value(num(2.5)),
                    FuncArg::Value(look),
                    FuncArg::Value(ret),
                    FuncArg::Value(CalcValue::Blank),
                    FuncArg::Value(num(1.0)),
                ]
            ),
            Ok(num(30.0))
        );
    }

    #[test]
    fn xlookup_wildcards_search_and_if_not_found() {
        let r = GridResolver::new();
        let look = col(vec![txt("apple"), txt("banana"), txt("cherry")]);
        let ret = col(vec![txt("x"), txt("y"), txt("z")]);
        assert_eq!(
            call(
                &XLOOKUP,
                &r,
                vec![
                    FuncArg::Value(txt("a*")),
                    FuncArg::Value(look.clone()),
                    FuncArg::Value(ret.clone()),
                    FuncArg::Value(CalcValue::Blank),
                    FuncArg::Value(num(2.0)),
                ]
            ),
            Ok(txt("x"))
        );
        let dup_look = col(vec![num(1.0), num(2.0), num(2.0), num(3.0)]);
        let dup_ret = col(vec![num(10.0), num(20.0), num(21.0), num(30.0)]);
        assert_eq!(
            call(
                &XLOOKUP,
                &r,
                vec![
                    FuncArg::Value(num(2.0)),
                    FuncArg::Value(dup_look),
                    FuncArg::Value(dup_ret),
                    FuncArg::Value(CalcValue::Blank),
                    FuncArg::Value(num(0.0)),
                    FuncArg::Value(num(-1.0)),
                ]
            ),
            Ok(num(21.0))
        );
        assert_eq!(
            call(
                &XLOOKUP,
                &r,
                vec![
                    FuncArg::Value(num(9.0)),
                    FuncArg::Value(look.clone()),
                    FuncArg::Value(ret.clone()),
                    FuncArg::Value(txt("nf")),
                ]
            ),
            Ok(txt("nf"))
        );
        assert_eq!(
            call(
                &XLOOKUP,
                &r,
                vec![
                    FuncArg::Value(num(9.0)),
                    FuncArg::Value(look.clone()),
                    FuncArg::Value(ret.clone()),
                ]
            ),
            Err(CalcError::Na)
        );
        assert_eq!(
            call(
                &XLOOKUP,
                &r,
                vec![
                    FuncArg::Value(num(1.0)),
                    FuncArg::Value(look),
                    FuncArg::Value(col(vec![num(1.0)])),
                ]
            ),
            Err(CalcError::Value)
        );
        assert_eq!(
            call(
                &XLOOKUP,
                &r,
                vec![
                    FuncArg::Value(num(1.0)),
                    FuncArg::Value(col(vec![num(1.0)])),
                    FuncArg::Value(col(vec![num(2.0)])),
                    FuncArg::Value(CalcValue::Blank),
                    FuncArg::Value(num(5.0)),
                ]
            ),
            Err(CalcError::Value)
        );
    }

    #[test]
    fn match_exact_approx_and_wildcards() {
        let r = GridResolver::new();
        let vals = col(vec![num(10.0), num(20.0), num(30.0)]);
        assert_eq!(
            call(
                &MATCH,
                &r,
                vec![
                    FuncArg::Value(num(20.0)),
                    FuncArg::Value(vals.clone()),
                    FuncArg::Value(num(0.0)),
                ]
            ),
            Ok(num(2.0))
        );
        let asc = col(vec![num(1.0), num(2.0), num(2.0), num(3.0)]);
        assert_eq!(
            call(
                &MATCH,
                &r,
                vec![
                    FuncArg::Value(num(2.0)),
                    FuncArg::Value(asc),
                    FuncArg::Value(num(1.0)),
                ]
            ),
            Ok(num(3.0))
        );
        let desc = col(vec![num(5.0), num(4.0), num(3.0), num(2.0)]);
        assert_eq!(
            call(
                &MATCH,
                &r,
                vec![
                    FuncArg::Value(num(3.0)),
                    FuncArg::Value(desc),
                    FuncArg::Value(num(-1.0)),
                ]
            ),
            Ok(num(3.0))
        );
        let words = col(vec![txt("apple"), txt("apricot"), txt("banana")]);
        assert_eq!(
            call(
                &MATCH,
                &r,
                vec![
                    FuncArg::Value(txt("ap*")),
                    FuncArg::Value(words),
                    FuncArg::Value(num(0.0)),
                ]
            ),
            Ok(num(1.0))
        );
        assert_eq!(
            call(
                &MATCH,
                &r,
                vec![
                    FuncArg::Value(num(40.0)),
                    FuncArg::Value(vals.clone()),
                    FuncArg::Value(num(0.0)),
                ]
            ),
            Err(CalcError::Na)
        );
        assert_eq!(
            call(
                &MATCH,
                &r,
                vec![
                    FuncArg::Value(num(0.0)),
                    FuncArg::Value(vals),
                    FuncArg::Value(num(1.0)),
                ]
            ),
            Err(CalcError::Na)
        );
    }

    #[test]
    fn match_exact_does_not_coerce_types() {
        let r = GridResolver::new();
        let mixed = col(vec![num(1.0), txt("2"), num(2.0)]);
        assert_eq!(
            call(
                &MATCH,
                &r,
                vec![
                    FuncArg::Value(txt("2")),
                    FuncArg::Value(mixed),
                    FuncArg::Value(num(0.0)),
                ]
            ),
            Ok(num(2.0))
        );
        assert_eq!(
            call(
                &MATCH,
                &r,
                vec![
                    FuncArg::Value(num(2.0)),
                    FuncArg::Value(col(vec![txt("1"), txt("2")])),
                    FuncArg::Value(num(0.0)),
                ]
            ),
            Err(CalcError::Na)
        );
    }

    #[test]
    fn match_propagates_array_errors() {
        let r = GridResolver::new();
        let vals = col(vec![CalcValue::err(CalcError::Na), num(1.0)]);
        assert_eq!(
            call(
                &MATCH,
                &r,
                vec![
                    FuncArg::Value(num(1.0)),
                    FuncArg::Value(vals),
                    FuncArg::Value(num(0.0)),
                ]
            ),
            Err(CalcError::Na)
        );
    }

    #[test]
    fn index_cell_rows_columns_and_errors() {
        let mut r = GridResolver::new();
        r.put(0, 0, num(1.0));
        r.put(0, 1, txt("a"));
        r.put(1, 0, num(2.0));
        r.put(1, 1, txt("b"));
        r.put(2, 0, num(3.0));
        r.put(2, 1, txt("c"));
        let tbl = FuncArg::Reference(range_ref(0, 0, 2, 1));
        assert_eq!(
            call(
                &INDEX,
                &r,
                vec![
                    tbl.clone(),
                    FuncArg::Value(num(2.0)),
                    FuncArg::Value(num(2.0)),
                ]
            ),
            Ok(txt("b"))
        );
        match call(
            &INDEX,
            &r,
            vec![
                tbl.clone(),
                FuncArg::Value(num(0.0)),
                FuncArg::Value(num(2.0)),
            ],
        ) {
            Ok(CalcValue::Array(a)) => {
                assert_eq!(a.shape(), (3, 1));
                assert_eq!(a.get(0, 0), &txt("a"));
                assert_eq!(a.get(2, 0), &txt("c"));
            }
            other => panic!("expected array, got {other:?}"),
        }
        match call(
            &INDEX,
            &r,
            vec![
                tbl.clone(),
                FuncArg::Value(num(2.0)),
                FuncArg::Value(num(0.0)),
            ],
        ) {
            Ok(CalcValue::Array(a)) => {
                assert_eq!(a.shape(), (1, 2));
                assert_eq!(a.get(0, 0), &num(2.0));
                assert_eq!(a.get(0, 1), &txt("b"));
            }
            other => panic!("expected array, got {other:?}"),
        }
        assert!(
            call(
                &INDEX,
                &r,
                vec![
                    tbl.clone(),
                    FuncArg::Value(num(0.0)),
                    FuncArg::Value(num(0.0)),
                ]
            )
            .map(|v| v.is_array())
            .unwrap()
        );
        let col_tbl = FuncArg::Reference(range_ref(0, 0, 2, 0));
        assert_eq!(
            call(&INDEX, &r, vec![col_tbl, FuncArg::Value(num(3.0))]),
            Ok(num(3.0))
        );
        assert!(
            call(&INDEX, &r, vec![tbl.clone(), FuncArg::Value(num(2.0))])
                .map(|v| v.is_array())
                .unwrap()
        );
        assert_eq!(
            call(
                &INDEX,
                &r,
                vec![
                    tbl.clone(),
                    FuncArg::Value(num(5.0)),
                    FuncArg::Value(num(1.0)),
                ]
            ),
            Err(CalcError::Ref)
        );
        assert_eq!(
            call(
                &INDEX,
                &r,
                vec![
                    tbl.clone(),
                    FuncArg::Value(num(1.0)),
                    FuncArg::Value(num(9.0)),
                ]
            ),
            Err(CalcError::Ref)
        );
        assert_eq!(
            call(
                &INDEX,
                &r,
                vec![tbl, FuncArg::Value(num(-1.0)), FuncArg::Value(num(1.0))]
            ),
            Err(CalcError::Value)
        );
    }

    #[test]
    fn lookup_vector_and_array_forms() {
        let r = GridResolver::new();
        let look = col(vec![num(1.0), num(3.0), num(5.0)]);
        let res = col(vec![num(10.0), num(30.0), num(50.0)]);
        assert_eq!(
            call(
                &LOOKUP,
                &r,
                vec![
                    FuncArg::Value(num(4.0)),
                    FuncArg::Value(look),
                    FuncArg::Value(res),
                ]
            ),
            Ok(num(30.0))
        );
        assert_eq!(
            call(
                &LOOKUP,
                &r,
                vec![
                    FuncArg::Value(num(4.0)),
                    FuncArg::Value(col(vec![num(1.0), num(2.0)])),
                    FuncArg::Value(col(vec![num(10.0)])),
                ]
            ),
            Err(CalcError::Value)
        );
        let tall = arr(
            3,
            2,
            vec![
                num(1.0),
                num(10.0),
                num(3.0),
                num(30.0),
                num(5.0),
                num(50.0),
            ],
        );
        assert_eq!(
            call(
                &LOOKUP,
                &r,
                vec![FuncArg::Value(num(4.0)), FuncArg::Value(tall)]
            ),
            Ok(num(30.0))
        );
        let wide = arr(
            2,
            3,
            vec![
                num(1.0),
                num(2.0),
                num(3.0),
                num(10.0),
                num(20.0),
                num(30.0),
            ],
        );
        assert_eq!(
            call(
                &LOOKUP,
                &r,
                vec![FuncArg::Value(num(2.0)), FuncArg::Value(wide)]
            ),
            Ok(num(20.0))
        );
        assert_eq!(
            call(
                &LOOKUP,
                &r,
                vec![
                    FuncArg::Value(num(0.0)),
                    FuncArg::Value(col(vec![num(1.0), num(3.0)])),
                    FuncArg::Value(col(vec![num(10.0), num(30.0)])),
                ]
            ),
            Err(CalcError::Na)
        );
    }

    #[test]
    fn choose_picks_by_one_based_index() {
        let r = GridResolver::new();
        assert_eq!(
            call(
                &CHOOSE,
                &r,
                vec![
                    FuncArg::Value(num(2.0)),
                    FuncArg::Value(txt("a")),
                    FuncArg::Value(txt("b")),
                    FuncArg::Value(txt("c")),
                ]
            ),
            Ok(txt("b"))
        );
        assert_eq!(
            call(
                &CHOOSE,
                &r,
                vec![FuncArg::Value(num(0.0)), FuncArg::Value(txt("a"))]
            ),
            Err(CalcError::Value)
        );
        assert_eq!(
            call(
                &CHOOSE,
                &r,
                vec![
                    FuncArg::Value(num(3.0)),
                    FuncArg::Value(txt("a")),
                    FuncArg::Value(txt("b")),
                ]
            ),
            Err(CalcError::Value)
        );
        assert_eq!(
            call(
                &CHOOSE,
                &r,
                vec![
                    FuncArg::Value(num(1.9)),
                    FuncArg::Value(num(10.0)),
                    FuncArg::Value(num(20.0)),
                ]
            ),
            Ok(num(10.0))
        );
    }

    // -----------------------------------------------------------------------
    // Reference-construction additions. These drive the REAL pipeline via
    // testkit (parse_formula then eval), matching the per-class matrices.
    // OFFSET and INDIRECT are value-form only: see their docstrings for the
    // architectural limit.
    // -----------------------------------------------------------------------
    mod ref_build {
        use super::*;
        use crate::turbo::calc::testkit::{Grid, Outcome, error, num, text};

        #[test]
        fn offset_returns_a_single_cell_value() {
            let g = Grid::empty().set_num("B2", 10.0);
            assert_eq!(g.num("=OFFSET(B2,0,0)"), 10.0);
            assert_eq!(g.num("=OFFSET(A1,1,1)"), 10.0);
            assert_eq!(g.num("=OFFSET(A1,1,1,1,1)"), 10.0);
        }

        #[test]
        fn offset_returns_a_dense_array() {
            let g = Grid::empty().col("A1", &[1.0, 2.0, 3.0, 4.0]);
            let a = g.array("=OFFSET(A1,1,0,2,1)");
            assert_eq!(a.shape(), (2, 1));
            assert_eq!(a.get(0, 0), &CalcValue::Number(2.0));
            assert_eq!(a.get(1, 0), &CalcValue::Number(3.0));

            let wide = Grid::empty().row("B1", &[5.0, 6.0, 7.0]);
            let a = wide.array("=OFFSET(B1,0,0,1,3)");
            assert_eq!(a.shape(), (1, 3));
            assert_eq!(a.get(0, 2), &CalcValue::Number(7.0));
        }

        #[test]
        fn offset_height_width_default_to_the_base_reference() {
            let g = Grid::empty()
                .set_num("A1", 1.0)
                .set_num("B1", 2.0)
                .set_num("A2", 3.0)
                .set_num("B2", 4.0);
            // a range base with no height/width keeps the range's own shape
            let a = g.array("=OFFSET(A1:B2,0,0)");
            assert_eq!(a.shape(), (2, 2));
            assert_eq!(a.get(1, 1), &CalcValue::Number(4.0));
            // height only: width defaults to 1 for a single-cell base
            let a = g.array("=OFFSET(A1,0,1,2)");
            assert_eq!(a.shape(), (2, 1));
            assert_eq!(a.get(1, 0), &CalcValue::Number(4.0));
            // an explicitly blank height is treated as omitted
            let a = g.array("=OFFSET(A1,0,0,,2)");
            assert_eq!(a.shape(), (1, 2));
            assert_eq!(a.get(0, 0), &CalcValue::Number(1.0));
            assert_eq!(a.get(0, 1), &CalcValue::Number(2.0));
        }

        #[test]
        fn offset_as_a_range_argument_to_sum() {
            let g = Grid::empty().col("A1", &[1.0, 2.0, 3.0, 4.0]);
            assert_eq!(g.num("=SUM(OFFSET(A1,1,0,3,1))"), 9.0);
            assert_eq!(g.num("=SUM(OFFSET(A1:A4,0,0))"), 10.0);
        }

        #[test]
        fn offset_off_the_grid_is_ref() {
            let g = Grid::empty().set_num("A1", 1.0);
            assert_eq!(g.error("=OFFSET(A1,-1,0)"), CalcError::Ref);
            assert_eq!(g.error("=OFFSET(A1,0,-1)"), CalcError::Ref);
            assert_eq!(g.error("=OFFSET(A1,0,16384)"), CalcError::Ref);
            assert_eq!(g.error("=OFFSET(A1,1048576,0)"), CalcError::Ref);
            // zero or negative height/width is #REF!, not a degenerate range
            assert_eq!(g.error("=OFFSET(A1,0,0,0,1)"), CalcError::Ref);
            assert_eq!(g.error("=OFFSET(A1,0,0,1,-2)"), CalcError::Ref);
            // a valid anchor whose extent runs past the last row
            assert_eq!(g.error("=OFFSET(A1,0,0,1048577,1)"), CalcError::Ref);
        }

        #[test]
        fn offset_rejects_non_reference_and_non_numeric_arguments() {
            let g = Grid::empty().set_num("A1", 1.0);
            assert_eq!(g.error("=OFFSET(1,0,0)"), CalcError::Value);
            assert_eq!(g.error("=OFFSET(A1,\"abc\",0)"), CalcError::Value);
            assert_eq!(g.error("=OFFSET(A1,0,0,\"x\",1)"), CalcError::Value);
            // errors in the offset arguments propagate
            assert_eq!(g.error("=OFFSET(A1,#N/A,0)"), CalcError::Na);
        }

        #[test]
        fn indirect_a1_style_cells_and_ranges() {
            let g = Grid::empty().set_num("A1", 10.0).set_num("B2", 20.0);
            assert_eq!(g.num("=INDIRECT(\"A1\")"), 10.0);
            assert_eq!(g.num("=INDIRECT(\"$B$2\")"), 20.0);
            assert_eq!(g.num("=INDIRECT(\"B2\",TRUE)"), 20.0);
            let a = g.array("=INDIRECT(\"A1:B1\")");
            assert_eq!(a.shape(), (1, 2));
            assert_eq!(a.get(0, 0), &CalcValue::Number(10.0));
            assert_eq!(a.get(0, 1), &CalcValue::Blank);
        }

        #[test]
        fn indirect_reads_the_reference_text_from_a_cell() {
            let g = Grid::empty().set_text("A1", "B2").set_num("B2", 42.0);
            assert_eq!(g.num("=INDIRECT(A1)"), 42.0);
        }

        #[test]
        fn indirect_sheet_qualified_references() {
            let g = Grid::empty().set_num("A1", 7.0);
            assert_eq!(g.num("=INDIRECT(\"Sheet1!A1\")"), 7.0);
            let a = g.array("=INDIRECT(\"Sheet1!A1:B1\")");
            assert_eq!(a.get(0, 0), &CalcValue::Number(7.0));
            assert_eq!(g.error("=INDIRECT(\"NoSuchSheet!A1\")"), CalcError::Ref);
            assert_eq!(g.error("=INDIRECT(\"'My Sheet'!A1\")"), CalcError::Ref);
            assert_eq!(g.error("=INDIRECT(\"Sheet1!\")"), CalcError::Ref);
        }

        #[test]
        fn indirect_r1c1_style_absolute_and_relative() {
            let g = Grid::empty().set_num("A1", 10.0).set_num("D1", 99.0);
            assert_eq!(g.num("=INDIRECT(\"R1C1\",FALSE)"), 10.0);
            assert_eq!(g.num("=INDIRECT(\"R1C4\",FALSE)"), 99.0);
            assert_eq!(g.num("=INDIRECT(\"R1C1\",\"FALSE\")"), 10.0);
            assert_eq!(g.num("=INDIRECT(\"R1C1\",0)"), 10.0);
            // relative R[-1]C[2] anchored at the formula cell B2 -> D1
            assert_eq!(
                g.at("B2", r#"=INDIRECT("R[-1]C[2]",FALSE)"#),
                Outcome::Value(CalcValue::Number(99.0))
            );
            // absolute row, relative column -> R1 + C[2] from B2 = D1
            assert_eq!(
                g.at("B2", r#"=INDIRECT("R1C[2]",FALSE)"#),
                Outcome::Value(CalcValue::Number(99.0))
            );
            // RC alone is the formula cell itself (empty here)
            assert_eq!(
                g.at("B2", r#"=INDIRECT("RC",FALSE)"#),
                Outcome::Value(CalcValue::Blank)
            );
        }

        #[test]
        fn indirect_unparseable_or_out_of_grid_is_ref() {
            let g = Grid::empty();
            assert_eq!(g.error("=INDIRECT(\"\")"), CalcError::Ref);
            assert_eq!(g.error("=INDIRECT(\"not a ref\")"), CalcError::Ref);
            assert_eq!(g.error("=INDIRECT(\"XFE1\")"), CalcError::Ref);
            assert_eq!(g.error("=INDIRECT(\"A1048577\")"), CalcError::Ref);
            assert_eq!(g.error("=INDIRECT(\"A1:B\")"), CalcError::Ref);
            // FALSE must parse as R1C1, never silently fall back to A1
            assert_eq!(g.error("=INDIRECT(\"A1\",FALSE)"), CalcError::Ref);
            assert_eq!(g.error("=INDIRECT(\"R1048577C1\",FALSE)"), CalcError::Ref);
            assert_eq!(g.error("=INDIRECT(\"R1C16385\",FALSE)"), CalcError::Ref);
            assert_eq!(g.error("=INDIRECT(\"R0C1\",FALSE)"), CalcError::Ref);
            assert_eq!(g.error("=INDIRECT(\"R1C1R\",FALSE)"), CalcError::Ref);
        }

        #[test]
        fn address_a1_style_covers_all_four_abs_num_values() {
            assert_eq!(text("=ADDRESS(2,3)"), "$C$2");
            assert_eq!(text("=ADDRESS(2,3,1)"), "$C$2");
            assert_eq!(text("=ADDRESS(2,3,2)"), "C$2");
            assert_eq!(text("=ADDRESS(2,3,3)"), "$C2");
            assert_eq!(text("=ADDRESS(2,3,4)"), "C2");
            assert_eq!(text("=ADDRESS(1048576,16384)"), "$XFD$1048576");
        }

        #[test]
        fn address_r1c1_style_covers_all_four_abs_num_values() {
            assert_eq!(text("=ADDRESS(2,3,1,FALSE)"), "R2C3");
            assert_eq!(text("=ADDRESS(2,3,2,FALSE)"), "R2C[3]");
            assert_eq!(text("=ADDRESS(2,3,3,FALSE)"), "R[2]C3");
            assert_eq!(text("=ADDRESS(2,3,4,FALSE)"), "R[2]C[3]");
        }

        #[test]
        fn address_quotes_a_sheet_name_with_spaces_or_apostrophes() {
            assert_eq!(text("=ADDRESS(1,1,1,TRUE,\"My Sheet\")"), "'My Sheet'!$A$1");
            assert_eq!(text("=ADDRESS(1,1,1,TRUE,\"O'Brien\")"), "'O''Brien'!$A$1");
            assert_eq!(text("=ADDRESS(1,1,1,TRUE,\"Data\")"), "Data!$A$1");
            assert_eq!(text("=ADDRESS(2,3,4,TRUE,\"My Sheet\")"), "'My Sheet'!C2");
        }

        #[test]
        fn address_errors() {
            assert_eq!(error("=ADDRESS(0,1)"), CalcError::Value);
            assert_eq!(error("=ADDRESS(1,0)"), CalcError::Value);
            assert_eq!(error("=ADDRESS(1048577,1)"), CalcError::Value);
            assert_eq!(error("=ADDRESS(1,16385)"), CalcError::Value);
            assert_eq!(error("=ADDRESS(1,1,0)"), CalcError::Value);
            assert_eq!(error("=ADDRESS(1,1,5)"), CalcError::Value);
            assert_eq!(error("=ADDRESS(\"abc\",1)"), CalcError::Value);
        }

        #[test]
        fn hyperlink_returns_the_display_value() {
            assert_eq!(
                text("=HYPERLINK(\"https://example.com\",\"click\")"),
                "click"
            );
            assert_eq!(
                text("=HYPERLINK(\"https://example.com\")"),
                "https://example.com"
            );
            assert_eq!(num("=HYPERLINK(\"https://example.com\",42)"), 42.0);
            let g = Grid::empty().set_text("A1", "label");
            assert_eq!(g.text("=HYPERLINK(\"https://example.com\",A1)"), "label");
        }

        #[test]
        fn hyperlink_arity() {
            assert_eq!(error("=HYPERLINK()"), CalcError::Value);
            assert_eq!(error("=HYPERLINK(1,2,3)"), CalcError::Value);
        }
    }

    // -----------------------------------------------------------------------
    // Round-2 additions: AREAS, FORMULATEXT, IMAGE, WRAPCOLS, WRAPROWS.
    // -----------------------------------------------------------------------
    mod round2_additions {
        use super::*;
        use crate::turbo::calc::testkit::{Grid, Outcome, error, text};

        #[test]
        fn areas_reports_one_area_for_any_reference() {
            let g = Grid::empty();
            assert_eq!(g.num("=AREAS(A1)"), 1.0);
            assert_eq!(g.num("=AREAS(A1:B2)"), 1.0);
            assert_eq!(g.num("=AREAS(A:A)"), 1.0);
            assert_eq!(g.num("=AREAS(Sheet1!A1)"), 1.0);
            assert_eq!(g.error("=AREAS(\"A1\")"), CalcError::Value);
            assert_eq!(g.error("=AREAS(5)"), CalcError::Value);
        }

        #[test]
        fn formulatext_is_na_for_value_grid_cells() {
            let g = Grid::empty().set_num("A1", 1.0);
            // The resolver exposes computed values only, so no cell is a formula.
            assert_eq!(g.error("=FORMULATEXT(A1)"), CalcError::Na);
            assert_eq!(g.error("=FORMULATEXT(Z99)"), CalcError::Na);
            assert_eq!(g.error("=FORMULATEXT(\"A1\")"), CalcError::Value);
            assert_eq!(g.error("=FORMULATEXT(1)"), CalcError::Value);
        }

        #[test]
        fn image_is_a_present_stub_that_errors() {
            let g = Grid::empty();
            assert_eq!(
                g.error("=IMAGE(\"https://example.com/pic.png\")"),
                CalcError::Value
            );
            assert_eq!(g.error("=IMAGE(\"u\",\"alt\",0,100,100)"), CalcError::Value);
        }

        #[test]
        fn wraprows_lays_the_vector_out_row_by_row() {
            let g = Grid::empty();
            let a = g.array("=WRAPROWS({1,2,3,4,5},2)");
            assert_eq!(a.shape(), (3, 2));
            assert_eq!(a.get(0, 0), &CalcValue::Number(1.0));
            assert_eq!(a.get(0, 1), &CalcValue::Number(2.0));
            assert_eq!(a.get(1, 0), &CalcValue::Number(3.0));
            assert_eq!(a.get(1, 1), &CalcValue::Number(4.0));
            assert_eq!(a.get(2, 0), &CalcValue::Number(5.0));
            assert_eq!(a.get(2, 1), &CalcValue::Error(CalcError::Na));

            // a column vector and an exact multiple need no padding
            let g = Grid::empty().col("A1", &[1.0, 2.0, 3.0, 4.0]);
            let a = g.array("=WRAPROWS(A1:A4,2)");
            assert_eq!(a.shape(), (2, 2));
            assert_eq!(a.get(1, 1), &CalcValue::Number(4.0));

            // a custom pad fills the gap
            let g = Grid::empty();
            let a = g.array("=WRAPROWS({1;2;3},2,0)");
            assert_eq!(a.shape(), (2, 2));
            assert_eq!(a.get(1, 1), &CalcValue::Number(0.0));
        }

        #[test]
        fn wrapcols_lays_the_vector_out_column_by_column() {
            let g = Grid::empty();
            let a = g.array("=WRAPCOLS({1,2,3,4,5},2)");
            assert_eq!(a.shape(), (2, 3));
            assert_eq!(a.get(0, 0), &CalcValue::Number(1.0));
            assert_eq!(a.get(1, 0), &CalcValue::Number(2.0));
            assert_eq!(a.get(0, 1), &CalcValue::Number(3.0));
            assert_eq!(a.get(1, 1), &CalcValue::Number(4.0));
            assert_eq!(a.get(0, 2), &CalcValue::Number(5.0));
            assert_eq!(a.get(1, 2), &CalcValue::Error(CalcError::Na));

            let g = Grid::empty();
            let a = g.array("=WRAPCOLS({1;2;3;4},2,0)");
            assert_eq!(a.shape(), (2, 2));
            assert_eq!(a.get(0, 0), &CalcValue::Number(1.0));
            assert_eq!(a.get(1, 1), &CalcValue::Number(4.0));
        }

        #[test]
        fn wrap_errors() {
            let g = Grid::empty();
            assert_eq!(g.error("=WRAPROWS({1,2},0)"), CalcError::Value);
            assert_eq!(g.error("=WRAPROWS({1,2},-1)"), CalcError::Value);
            assert_eq!(g.error("=WRAPCOLS({1,2},\"x\")"), CalcError::Value);
            // a 2-D array cannot be wrapped
            assert_eq!(g.error("=WRAPROWS({1,2;3,4},2)"), CalcError::Value);
            // an empty vector -> #CALC! (direct call; `{}` is not parseable)
            let empty = FuncArg::Value(CalcValue::array(ArrayValue::new(0, 0, Vec::new())));
            let ctx = FuncCtx {
                date1904: false,
                sheet: 0,
                row: 0,
                col: 0,
                resolver: &Grid::empty(),
            };
            assert_eq!(
                (WRAPROWS.func)(&ctx, &[empty, FuncArg::Value(CalcValue::Number(2.0))]),
                Err(CalcError::Calc)
            );
        }

        #[test]
        fn lane_g_round2_pins() {
            // the fail-row forms that must evaluate, plus the add-missing
            // functions' presence in the live registry
            let g = Grid::empty().set_num("A1", 1.0);
            assert_eq!(text("=ADDRESS(2,3)"), "$C$2");
            assert_eq!(g.boolean("=ISREF(A1)"), true);
            let r = crate::turbo::calc::functions::registry();
            for name in [
                "AREAS",
                "FORMULATEXT",
                "IMAGE",
                "WRAPCOLS",
                "WRAPROWS",
                "ISFORMULA",
                "SHEETS",
                "ENCODEURL",
            ] {
                assert!(r.get(name).is_some(), "{name} must be registered");
            }
            // CHOOSE already returns an array value for an array constant
            match g.calc("=CHOOSE(1,{2;3;4})") {
                Outcome::Value(CalcValue::Array(a)) => {
                    assert_eq!(a.shape(), (3, 1));
                    assert_eq!(a.get(0, 0), &CalcValue::Number(2.0));
                }
                other => panic!("=CHOOSE(1,{{2;3;4}}) -> {other:?}"),
            }
        }
    }
}
