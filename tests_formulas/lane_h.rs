//! Lane H — array-result hydration / spill persistence
//! (`formula-validation/round2/brief_lane_H.csv`).
//!
//! Every formula family from the brief is evaluated through the real write path
//! (`hydrate_workbook` on a `write::model::Workbook`), then the saved artifact
//! is checked three ways:
//!   1. the model after hydration: the anchor keeps its formula and gets a
//!      cached `<v>`, and every spilled element became a plain typed cell;
//!   2. the worksheet XML: exactly one `<f>` (the anchor), correct `t=`/
//!      values for every cell, and never a non-cacheable `t="e"` code;
//!   3. the whole package: `write_workbook_bytes` inflates as a valid zip with
//!      the sheet part present (the full Excel-open check is Lane J's COM
//!      test; this lane re-reads with kyrax in `lane_H_validate.py`).
//!
//! The byte-identity contract (`scalar_workbook_output_is_byte_identical`)
//! freezes the writer's output for a workbook with zero array formulas, so the
//! spill work is proven not to disturb anything else.

use kyrax::turbo::calc::{CalcOptions, hydrate_workbook};
use kyrax::turbo::write::{
    CachedValue, Cell, CellValue, FormulaKind, Row, SstBuilder, Workbook, write_workbook_bytes,
    write_worksheet,
};
use pretty_assertions::assert_eq;

use std::io::Read;

// ---------------------------------------------------------------------------
// Model construction
// ---------------------------------------------------------------------------

/// Build a `CellValue` from a compact spec: `"n:2.5"`, `"s:Tom"`, `"b:true"`,
/// `"e:#N/A"`.
fn cval(spec: &str) -> CellValue {
    let (t, rest) = spec.split_once(':').expect("spec needs a ':'");
    match t {
        "n" => CellValue::Number(rest.parse().expect("number spec")),
        "s" => CellValue::Str(rest.to_string()),
        "b" => CellValue::Bool(rest == "true"),
        "e" => CellValue::Error(rest.to_string()),
        _ => panic!("bad spec {spec}"),
    }
}

fn push_cell(rows: &mut Vec<Row>, r: u32, c: u32, v: CellValue) {
    match rows.iter_mut().find(|row| row.row == r) {
        Some(row) => match row.cells.binary_search_by(|cell| cell.col.cmp(&c)) {
            Ok(_) => panic!("duplicate cell R{r}C{c}"),
            Err(pos) => row.cells.insert(pos, Cell::new(c, v)),
        },
        None => {
            let mut row = Row::new(r);
            row.cells.push(Cell::new(c, v));
            rows.push(row);
        }
    }
}

struct Case<'a> {
    name: &'a str,
    formula: &'a str,
    /// 1-based anchor of the formula.
    anchor: (u32, u32),
    /// Seed cells: (row, col, spec).
    data: &'a [(u32, u32, &'a str)],
    /// Expected anchor cache spec; `"n:?"` means "some number".
    anchor_expected: &'a str,
    /// Expected spilled cells: (row, col, spec).
    spill: &'a [(u32, u32, &'a str)],
}

fn build_workbook(c: &Case) -> Workbook {
    let mut rows: Vec<Row> = Vec::new();
    for (r, col, spec) in c.data {
        push_cell(&mut rows, *r, *col, cval(spec));
    }
    push_cell(
        &mut rows,
        c.anchor.0,
        c.anchor.1,
        CellValue::Formula {
            text: c.formula.to_string(),
            kind: FormulaKind::Normal,
            cached: None,
        },
    );
    rows.sort_by_key(|r| r.row);
    let mut wb = Workbook::new();
    wb.sheets[0].name = "Sheet1".into();
    wb.sheets[0].rows = rows;
    wb
}

fn find_cell(wb: &Workbook, r: u32, c: u32) -> Option<&Cell> {
    let s = &wb.sheets[0];
    let row = s.rows.iter().find(|x| x.row == r)?;
    row.cells.iter().find(|x| x.col == c)
}

// ---------------------------------------------------------------------------
// Assertions
// ---------------------------------------------------------------------------

fn assert_cached(cached: Option<&CachedValue>, spec: &str, name: &str) {
    let (t, want) = spec.split_once(':').unwrap();
    if want == "?" {
        assert!(cached.is_some(), "{name}: expected a cached value");
        return;
    }
    match (t, cached) {
        ("n", Some(CachedValue::Number(n))) => {
            assert_eq!(
                *n,
                want.parse::<f64>().unwrap(),
                "{name}: anchor cached number"
            )
        }
        // `a` = approximate (relative tolerance 1e-6) — for statistical fits
        // whose least-squares arithmetic lands on a slightly inexact float.
        ("a", Some(CachedValue::Number(n))) => {
            let exp = want.parse::<f64>().unwrap();
            let rel = (n - exp).abs().max(1e-6);
            assert!(
                (n - exp).abs() <= 1e-6 * rel,
                "{name}: anchor cached number {n} vs {exp}"
            );
        }
        ("s", Some(CachedValue::Str(s))) => {
            assert_eq!(s, want, "{name}: anchor cached string")
        }
        ("b", Some(CachedValue::Bool(b))) => {
            assert_eq!(*b, want == "true", "{name}: anchor cached bool")
        }
        ("e", Some(CachedValue::Error(e))) => {
            assert_eq!(e, want, "{name}: anchor cached error")
        }
        _ => panic!("{name}: anchor cache mismatch spec={spec} got={cached:?}"),
    }
}

fn approx_num(got: f64, want: f64) -> bool {
    (got - want).abs() <= 1e-6 * (got - want).abs().max(want.abs()).max(1.0)
}

fn assert_spec(v: &CellValue, spec: &str, name: &str, r: u32, c: u32) {
    let (t, want) = spec.split_once(':').unwrap();
    if want == "?" {
        return;
    }
    match (t, v) {
        ("n", CellValue::Number(n)) => {
            assert_eq!(*n, want.parse::<f64>().unwrap(), "{name} R{r}C{c}")
        }
        ("a", CellValue::Number(n)) => {
            assert!(
                approx_num(*n, want.parse::<f64>().unwrap()),
                "{name} R{r}C{c}: {n} vs {want}"
            );
        }
        ("s", CellValue::Str(s)) => assert_eq!(s, want, "{name} R{r}C{c}"),
        ("b", CellValue::Bool(b)) => assert_eq!(*b, want == "true", "{name} R{r}C{c}"),
        ("e", CellValue::Error(e)) => assert_eq!(e, want, "{name} R{r}C{c}"),
        _ => panic!("{name} R{r}C{c}: mismatch spec={spec} got={v:?}"),
    }
}

// ---------------------------------------------------------------------------
// Minimal worksheet-XML cell parser (only what these tests assert)
// ---------------------------------------------------------------------------

struct XmlCell {
    r: String,
    t: String,
    f: Option<String>,
    v: Option<String>,
}

fn find_sub(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| from + p)
}

fn find_byte(hay: &[u8], b: u8, from: usize) -> Option<usize> {
    if from >= hay.len() {
        return None;
    }
    hay[from..].iter().position(|&x| x == b).map(|p| from + p)
}

fn get_attr(s: &str, name: &str) -> Option<String> {
    let pat = format!("{name}=\"");
    let i = s.find(&pat)?;
    let rest = &s[i + pat.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn extract_tag(body: &str, prefix: &str) -> Option<String> {
    let s = body.find(prefix)?;
    let after = &body[s + prefix.len()..];
    if after.starts_with("/>") {
        return None;
    }
    let gt = after.find('>')?;
    let content = &after[gt + 1..];
    if content.is_empty() {
        return None;
    }
    // The close tag is the FIRST `</` after the value; anything later belongs
    // to an outer element (e.g. `<is><t>..</t></is>`).
    let end = content.find("</")?;
    let txt = &content[..end];
    if txt.is_empty() {
        None
    } else {
        Some(txt.to_string())
    }
}

fn parse_cells(xml: &str) -> Vec<XmlCell> {
    let b = xml.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(cstart) = find_sub(b, b"<c ", i) {
        let Some(gt) = find_byte(b, b'>', cstart) else {
            break;
        };
        let attrs = &xml[cstart + 3..gt];
        let self_closed = b[gt - 1] == b'/';
        let (body, next) = if self_closed {
            (String::new(), gt + 1)
        } else {
            let Some(close) = find_sub(b, b"</c>", gt + 1) else {
                break;
            };
            (xml[gt + 1..close].to_string(), close + 4)
        };
        let r = get_attr(attrs, "r").unwrap_or_default();
        let t = get_attr(attrs, "t").unwrap_or_default();
        let f = extract_tag(&body, "<f");
        let v = extract_tag(&body, "<v").or_else(|| {
            // Plain string cells use inlineStr: the text lives in `<is><t>`,
            // not `<v>`.
            if t == "inlineStr" {
                extract_tag(&body, "<t")
            } else {
                None
            }
        });
        out.push(XmlCell { r, t, f, v });
        i = next;
    }
    out
}

fn coord(r: u32, c: u32) -> String {
    let mut s = String::new();
    let mut c = c;
    while c > 0 {
        c -= 1;
        s.insert(0, (b'A' + (c % 26) as u8) as char);
        c /= 26;
    }
    s.push_str(&r.to_string());
    s
}

fn ref_to_rc(s: &str) -> (u32, u32) {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        i += 1;
    }
    let mut col = 0u32;
    for &b in &bytes[..i] {
        col = col * 26 + (b - b'A') as u32 + 1;
    }
    let row: u32 = s[i..].parse().expect("row digits");
    (row, col)
}

const CACHEABLE_ERRORS: [&str; 9] = [
    "#NULL!", "#DIV/0!", "#VALUE!", "#REF!", "#NAME?", "#NUM!", "#N/A", "#SPILL!", "#CALC!",
];

fn assert_xml_value(xc: &XmlCell, spec: &str, name: &str) {
    let (t, want) = spec.split_once(':').unwrap();
    match t {
        "n" if want == "?" => assert_eq!(xc.t, "", "{name} {}: number cell must be untyped", xc.r),
        "n" => {
            let got: f64 = xc.v.as_deref().unwrap_or("").parse().expect("numeric <v>");
            let exp: f64 = want.parse().unwrap();
            assert_eq!(got, exp, "{name} {}: number value", xc.r);
        }
        "a" => {
            let got: f64 = xc.v.as_deref().unwrap_or("").parse().expect("numeric <v>");
            let exp: f64 = want.parse().unwrap();
            assert!(
                approx_num(got, exp),
                "{name} {}: approximate number {got} vs {exp}",
                xc.r
            );
        }
        "s" => {
            assert!(
                xc.t == "str" || xc.t == "inlineStr",
                "{name} {}: string needs t=str|inlineStr, got t={}",
                xc.r,
                xc.t
            );
            assert_eq!(
                xc.v.as_deref().unwrap_or(""),
                want,
                "{name} {}: string value",
                xc.r
            );
        }
        "b" => {
            assert_eq!(xc.t, "b", "{name} {}: bool t attr", xc.r);
            let got = xc.v.as_deref().unwrap_or("");
            assert_eq!(
                got,
                if want == "true" { "1" } else { "0" },
                "{name} {}: bool",
                xc.r
            );
        }
        "e" => {
            assert_eq!(xc.t, "e", "{name} {}: error t attr", xc.r);
            assert_eq!(
                xc.v.as_deref().unwrap_or(""),
                want,
                "{name} {}: error",
                xc.r
            );
        }
        _ => panic!("bad spec {spec}"),
    }
}

// ---------------------------------------------------------------------------
// The case runner
// ---------------------------------------------------------------------------

fn run_case(c: &Case) {
    let mut wb = build_workbook(c);
    let opts = CalcOptions {
        force_recalc: true,
        ..Default::default()
    };
    let report = hydrate_workbook(&mut wb, &opts);
    assert_eq!(
        report.fallback, 0,
        "{}: unexpected fallback {:?}",
        c.name, report
    );
    assert_eq!(
        report.computed, 1,
        "{}: exactly the anchor is computed",
        c.name
    );

    // 1. model assertions
    let (ar, ac) = c.anchor;
    let anchor_cell =
        find_cell(&wb, ar, ac).unwrap_or_else(|| panic!("{}: anchor missing", c.name));
    match &anchor_cell.value {
        CellValue::Formula { text, cached, .. } => {
            assert_eq!(
                text.trim_start_matches('='),
                c.formula.trim_start_matches('='),
                "{}: anchor formula text",
                c.name
            );
            assert_cached(cached.as_ref(), c.anchor_expected, c.name);
        }
        other => panic!("{}: anchor is not a formula: {other:?}", c.name),
    }
    for (r, col, spec) in c.spill {
        let cell = find_cell(&wb, *r, *col)
            .unwrap_or_else(|| panic!("{}: spill R{}C{} missing from model", c.name, r, col));
        assert_spec(&cell.value, spec, c.name, *r, *col);
    }

    // 2. worksheet XML assertions
    let xml = write_worksheet(&wb.sheets[0], false, true, &mut SstBuilder::new());
    let xml = String::from_utf8(xml).expect("valid utf-8 worksheet xml");
    let cells = parse_cells(&xml);
    let formula_cells: Vec<&XmlCell> = cells.iter().filter(|x| x.f.is_some()).collect();
    assert_eq!(
        formula_cells.len(),
        1,
        "{}: only the anchor carries a formula",
        c.name
    );
    assert_eq!(
        ref_to_rc(&formula_cells[0].r),
        c.anchor,
        "{}: formula anchor",
        c.name
    );
    let anchor_xml = cells
        .iter()
        .find(|x| x.r == coord(ar, ac))
        .expect("anchor in xml");
    assert_xml_value(anchor_xml, c.anchor_expected, c.name);
    for (r, col, spec) in c.spill {
        let xc = cells
            .iter()
            .find(|x| x.r == coord(*r, *col))
            .unwrap_or_else(|| panic!("{}: spill {} missing from xml", c.name, coord(*r, *col)));
        assert_xml_value(xc, spec, c.name);
    }
    for xc in &cells {
        if xc.t == "e" {
            let v = xc.v.as_deref().unwrap_or("");
            assert!(
                CACHEABLE_ERRORS.contains(&v),
                "{}: illegal cached error code {v}",
                c.name
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The brief's families (brief_lane_H.csv, 35 rows + SEQUENCE/RANDARRAY/STEYX)
// ---------------------------------------------------------------------------

#[rustfmt::skip]
const CASES: &[Case] = &[
    // -- lookup family --
    Case { name: "CHOOSE", formula: "=CHOOSE(1,{2;3;4})", anchor: (1,1), data: &[], anchor_expected: "n:2",
        spill: &[(2,1,"n:3"),(3,1,"n:4")] },
    Case { name: "CHOOSECOLS", formula: "=CHOOSECOLS({1,2,3;2,3,4},1,2)", anchor: (1,1), data: &[], anchor_expected: "n:1",
        spill: &[(1,2,"n:2"),(2,1,"n:2"),(2,2,"n:3")] },
    Case { name: "CHOOSEROWS", formula: "=CHOOSEROWS({1,2,3;2,3,4},1,2)", anchor: (1,1), data: &[], anchor_expected: "n:1",
        spill: &[(1,2,"n:2"),(1,3,"n:3"),(2,1,"n:2"),(2,2,"n:3"),(2,3,"n:4")] },
    Case { name: "DROP", formula: "=DROP({2,2;2,2},1,1)", anchor: (1,1), data: &[], anchor_expected: "n:2",
        spill: &[] },
    Case { name: "EXPAND", formula: "=EXPAND({2,2;2,2},3,3)", anchor: (1,1), data: &[], anchor_expected: "n:2",
        spill: &[(1,2,"n:2"),(1,3,"e:#N/A"),(2,1,"n:2"),(2,2,"n:2"),(2,3,"e:#N/A"),
                 (3,1,"e:#N/A"),(3,2,"e:#N/A"),(3,3,"e:#N/A")] },
    Case { name: "FILTER", formula: "=FILTER({1,2,3;2,3,4},{true;false})", anchor: (1,1), data: &[], anchor_expected: "n:1",
        spill: &[(1,2,"n:2"),(1,3,"n:3")] },
    Case { name: "HSTACK", formula: "=HSTACK(\"a\",\"b\",\"c\",\"d\")", anchor: (1,1), data: &[], anchor_expected: "s:a",
        spill: &[(1,2,"s:b"),(1,3,"s:c"),(1,4,"s:d")] },
    Case { name: "SORT", formula: "=SORT({\"Year\",0;2,1;0,3;11,4;true,5;\"abc\",67;\"test\",8;#NAME?,11;false,2;2,222},1,1,FALSE)",
        anchor: (1,1), data: &[], anchor_expected: "n:0",
        spill: &[(1,2,"n:3"),(2,1,"n:2"),(2,2,"n:1"),(3,1,"n:2"),(3,2,"n:222"),(4,1,"n:11"),(4,2,"n:4"),
                 (5,1,"s:abc"),(5,2,"n:67"),(6,1,"s:test"),(6,2,"n:8"),(7,1,"s:Year"),(7,2,"n:0"),
                 (8,1,"b:false"),(8,2,"n:2"),(9,1,"b:true"),(9,2,"n:5"),(10,1,"e:#NAME?"),(10,2,"n:11")] },
    Case { name: "SORTBY", formula: "=SORTBY({11,\"Year\",0;13,2,1;15,0,3;17,11,4;19,true,5;21,\"abc\",67;23,\"test\",8;25,#NAME?,11;27,false,2;29,2,222},{11;2;3;4;7;4;7;4;9;10},1)",
        anchor: (1,1), data: &[], anchor_expected: "n:13",
        spill: &[(1,2,"n:2"),(1,3,"n:1"),(2,1,"n:15"),(2,2,"n:0"),(2,3,"n:3"),
                 (3,1,"n:17"),(3,2,"n:11"),(3,3,"n:4"),(4,1,"n:21"),(4,2,"s:abc"),(4,3,"n:67"),
                 (5,1,"n:25"),(5,2,"e:#NAME?"),(5,3,"n:11"),(6,1,"n:19"),(6,2,"b:true"),(6,3,"n:5"),
                 (7,1,"n:23"),(7,2,"s:test"),(7,3,"n:8"),(8,1,"n:27"),(8,2,"b:false"),(8,3,"n:2"),
                 (9,1,"n:29"),(9,2,"n:2"),(9,3,"n:222"),(10,1,"n:11"),(10,2,"s:Year"),(10,3,"n:0")] },
    Case { name: "TAKE", formula: "=TAKE({2,2;2,2},1,1)", anchor: (1,1), data: &[], anchor_expected: "n:2",
        spill: &[] },
    Case { name: "TOCOL", formula: "=TOCOL(A1:D3,3,0)", anchor: (1,6), data: &[
        (1,1,"s:Ben"),(2,1,"n:1"),(3,1,"s:Mary"),(1,2,"n:-2"),(2,2,"b:true"),(3,2,"s:James"),
        (1,3,"b:false"),(2,3,"s:Harry"),(3,3,"n:1.23"),(1,4,"s:彭德威")], anchor_expected: "s:Ben",
        spill: &[(2,6,"n:-2"),(3,6,"b:false"),(4,6,"s:彭德威"),(5,6,"n:1"),(6,6,"b:true"),
                 (7,6,"s:Harry"),(8,6,"s:Mary"),(9,6,"s:James"),(10,6,"n:1.23")] },
    Case { name: "TOROW", formula: "=TOROW(A1:D3,3,0)", anchor: (1,6), data: &[
        (1,1,"s:Ben"),(2,1,"n:1"),(3,1,"s:Mary"),(1,2,"n:-2"),(2,2,"b:true"),(3,2,"s:James"),
        (1,3,"b:false"),(2,3,"s:Harry"),(3,3,"n:1.23"),(1,4,"s:彭德威")], anchor_expected: "s:Ben",
        spill: &[(1,7,"n:-2"),(1,8,"b:false"),(1,9,"s:彭德威"),(1,10,"n:1"),(1,11,"b:true"),
                 (1,12,"s:Harry"),(1,13,"s:Mary"),(1,14,"s:James"),(1,15,"n:1.23")] },
    Case { name: "TRANSPOSE", formula: "=TRANSPOSE(A1:B10)", anchor: (1,3), data: &[
        (1,1,"s:Year"),(2,1,"n:2"),(3,1,"n:0"),(4,1,"n:11"),(5,1,"b:true"),(6,1,"s:abc"),(7,1,"s:test"),
        (8,1,"e:#NAME?"),(9,1,"b:false"),(10,1,"n:2"),(1,2,"n:0"),(2,2,"n:1"),(3,2,"n:3"),(4,2,"n:4"),
        (5,2,"n:5"),(6,2,"n:67"),(7,2,"n:8"),(8,2,"n:11"),(9,2,"n:2"),(10,2,"n:222")],
        anchor_expected: "s:Year",
        spill: &[(1,4,"n:2"),(1,5,"n:0"),(1,6,"n:11"),(1,7,"b:true"),(1,8,"s:abc"),(1,9,"s:test"),
                 (1,10,"e:#NAME?"),(1,11,"b:false"),(1,12,"n:2"),(2,3,"n:0"),(2,4,"n:1"),(2,5,"n:3"),
                 (2,6,"n:4"),(2,7,"n:5"),(2,8,"n:67"),(2,9,"n:8"),(2,10,"n:11"),(2,11,"n:2"),(2,12,"n:222")] },
    Case { name: "UNIQUE", formula: "=UNIQUE({2,2;2,2})", anchor: (1,1), data: &[], anchor_expected: "n:2",
        spill: &[(1,2,"n:2")] },
    Case { name: "VSTACK", formula: "=VSTACK(\"a\",\"b\",\"c\",\"d\")", anchor: (1,1), data: &[], anchor_expected: "s:a",
        spill: &[(2,1,"s:b"),(3,1,"s:c"),(4,1,"s:d")] },
    Case { name: "XLOOKUP", formula: "=XLOOKUP(5,{1;2;3;4;5;6},{\"First\",100,89;\"Second\",68,66;\"Third\",100,75;\"Fourth\",93,70;\"Fifth\",87,69;\"Sixth\",96,82})",
        anchor: (1,1), data: &[], anchor_expected: "s:Fifth",
        spill: &[(1,2,"n:87"),(1,3,"n:69")] },
    Case { name: "XMATCH", formula: "=XMATCH({\"Sixth\";\"First\";\"Fourth\"},{\"First\";\"Second\";\"Third\";\"Fourth\";\"Fifth\";\"Sixth\"})",
        anchor: (1,1), data: &[], anchor_expected: "n:6",
        spill: &[(2,1,"n:1"),(3,1,"n:4")] },

    // -- math family --
    Case { name: "MUNIT", formula: "=MUNIT(3)", anchor: (1,1), data: &[], anchor_expected: "n:1",
        spill: &[(1,2,"n:0"),(1,3,"n:0"),(2,1,"n:0"),(2,2,"n:1"),(2,3,"n:0"),(3,1,"n:0"),(3,2,"n:0"),(3,3,"n:1")] },
    Case { name: "RANDARRAY", formula: "=RANDARRAY(3,2)", anchor: (1,1), data: &[], anchor_expected: "n:?",
        spill: &[(1,2,"n:?"),(2,1,"n:?"),(2,2,"n:?"),(3,1,"n:?"),(3,2,"n:?")] },
    Case { name: "SEQUENCE", formula: "=SEQUENCE(3)", anchor: (1,1), data: &[], anchor_expected: "n:1",
        spill: &[(2,1,"n:2"),(3,1,"n:3")] },

    // -- statistical family --
    Case { name: "FREQUENCY", formula: "=FREQUENCY(A1:A6,B1:B2)", anchor: (1,3), data: &[
        (1,1,"n:1"),(2,1,"n:2"),(3,1,"n:3"),(4,1,"n:4"),(5,1,"n:5"),(6,1,"n:6"),(1,2,"n:2"),(2,2,"n:4")],
        anchor_expected: "n:2", spill: &[(2,3,"n:2"),(3,3,"n:2")] },
    Case { name: "GROWTH", formula: "=GROWTH(A1:A3,B1:B3,C1:C2)", anchor: (1,5), data: &[
        (1,1,"n:2"),(2,1,"n:4"),(3,1,"n:6"),(1,2,"n:1"),(2,2,"n:2"),(3,2,"n:3"),(1,3,"n:4"),(2,3,"n:5")],
        anchor_expected: "n:?", spill: &[(2,5,"n:?")] },
    Case { name: "LINEST", formula: "=LINEST(A1:A3,B1:B3)", anchor: (1,5), data: &[
        (1,1,"n:3"),(2,1,"n:5"),(3,1,"n:7"),(1,2,"n:1"),(2,2,"n:2"),(3,2,"n:3")],
        anchor_expected: "a:2", spill: &[(1,6,"a:1")] },
    Case { name: "LOGEST", formula: "=LOGEST(A1:A3,B1:B3)", anchor: (1,5), data: &[
        (1,1,"n:2"),(2,1,"n:4"),(3,1,"n:8"),(1,2,"n:1"),(2,2,"n:2"),(3,2,"n:3")],
        anchor_expected: "n:?", spill: &[(1,6,"n:?")] },
    Case { name: "MODE.MULT", formula: "=MODE.MULT(A1:A6)", anchor: (1,3), data: &[
        (1,1,"n:1"),(2,1,"n:1"),(3,1,"n:2"),(4,1,"n:2"),(5,1,"n:3"),(6,1,"n:4")],
        anchor_expected: "n:1", spill: &[(2,3,"n:2")] },
    Case { name: "STEYX", formula: "=STEYX(A1:B3, C1:D3)", anchor: (1,6), data: &[
        (1,1,"n:1"),(2,1,"n:2"),(3,1,"n:3"),(1,2,"n:3"),(2,2,"n:5"),(3,2,"n:7"),
        (1,3,"n:10"),(2,3,"n:11"),(3,3,"n:12"),(1,4,"n:13"),(2,4,"n:14"),(3,4,"n:15")],
        anchor_expected: "n:?", spill: &[] },
    Case { name: "TREND", formula: "=TREND(A1:A3,B1:B3,C1:C2)", anchor: (1,5), data: &[
        (1,1,"n:3"),(2,1,"n:5"),(3,1,"n:7"),(1,2,"n:1"),(2,2,"n:2"),(3,2,"n:3"),(1,3,"n:4"),(2,3,"n:5")],
        anchor_expected: "a:9", spill: &[(2,5,"a:11")] },

    // -- text family --
    Case { name: "TEXTSPLIT", formula: "=TEXTSPLIT(\"a,b,c\",\",\")", anchor: (1,1), data: &[], anchor_expected: "s:a",
        spill: &[(1,2,"s:b"),(1,3,"s:c")] },
];

// ---------------------------------------------------------------------------
// H.3: every brief family persists correctly
// ---------------------------------------------------------------------------

#[test]
fn every_brief_family_persists_anchor_and_spill() {
    for c in CASES {
        run_case(c);
    }
}

/// Two pre-existing engine families deliberately take the safe fallback on the
/// write path, so they are covered here as "never fabricated" rather than as
/// persistence cases:
///   * the LAMBDA family (BYCOL/BYROW/MAP/SCAN/REDUCE/MAKEARRAY) is present on
///     disk (`calc/lambda.rs`) but not wired into the function registry in
///     this build (`mod lambda` undeclared in `calc/mod.rs`);
///   * the reference-result family (INDEX/INDIRECT/OFFSET) IS computable by
///     `eval` but `calc/deps.rs` marks address functions unresolved, so the
///     dependency graph excludes them from the hydration order.
///
/// Both end in `fallback`, no cache is fabricated, and `fullCalcOnLoad` makes
/// Excel fill them on open. These are calc-engine/graph files outside Lane H's
/// ownership; this test pins the current (correct, non-fabricated) behaviour.
#[test]
fn address_and_lambda_families_stay_safe_fallback() {
    let opts = CalcOptions {
        force_recalc: true,
        ..Default::default()
    };
    let cases = [
        (
            "BYCOL",
            "=BYCOL({1,2,3},LAMBDA(x,x*2))",
            &[] as &[(u32, u32, &str)],
        ),
        ("BYROW", "=BYROW({1;2;3},LAMBDA(x,x*2))", &[]),
        ("MAKEARRAY", "=MAKEARRAY(4,4,LAMBDA(x,y,x*y))", &[]),
        ("MAP", "=MAP({1;2;3},LAMBDA(x,x*2))", &[]),
        ("REDUCE", "=REDUCE(1,{1;2;3},LAMBDA(x,y,x*y))", &[]),
        ("SCAN", "=SCAN(1,{1;2;3},LAMBDA(x,y,x*y))", &[]),
        ("INDEX", "=INDEX(A6:B7,1,1)", &[(6, 1, "s:Tom")]),
        ("INDIRECT", "=INDIRECT(\"B2\")", &[(2, 2, "n:4")]),
        ("OFFSET", "=OFFSET(A1,1,0,1,1)", &[(2, 1, "n:3")]),
    ];
    for (name, formula, data) in cases {
        let case = Case {
            name,
            formula,
            anchor: (3, 3),
            data,
            anchor_expected: "n:0",
            spill: &[],
        };
        let mut wb = build_workbook(&case);
        let report = hydrate_workbook(&mut wb, &opts);
        assert_eq!(
            report.computed, 0,
            "{name}: expected the engine to NOT compute this family on the write path"
        );
        assert_eq!(
            report.fallback, 1,
            "{name}: expected a clean fallback (fullCalcOnLoad recomputes)"
        );
        assert_eq!(
            find_cell(&wb, 3, 3).and_then(|c| match &c.value {
                CellValue::Formula { cached, .. } => cached.as_ref().map(|_| ()),
                _ => None,
            }),
            None,
            "{name}: a fallback family must never receive a fabricated cache"
        );
    }
}

// ---------------------------------------------------------------------------
// H.4 proxy: the saved package is a well-formed zip with the sheet part
// ---------------------------------------------------------------------------

#[test]
fn every_spilled_family_saves_into_a_readable_package() {
    let opts = CalcOptions {
        force_recalc: true,
        ..Default::default()
    };
    for c in CASES {
        let mut wb = build_workbook(c);
        hydrate_workbook(&mut wb, &opts);
        let bytes = write_workbook_bytes(&wb).expect("workbook serializes");
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
            .unwrap_or_else(|e| panic!("{}: package is not a readable zip: {e}", c.name));
        let mut sheet_xml = String::new();
        archive
            .by_name("xl/worksheets/sheet1.xml")
            .unwrap_or_else(|e| panic!("{}: sheet1.xml missing: {e}", c.name))
            .read_to_string(&mut sheet_xml)
            .expect("inflates");
        let cells = parse_cells(&sheet_xml);
        assert!(cells.len() > c.spill.len(), "{}: too few cells", c.name);
        let anchor = cells
            .iter()
            .find(|x| x.r == coord(c.anchor.0, c.anchor.1))
            .expect("anchor present in package");
        assert!(
            anchor.f.is_some(),
            "{}: anchor formula survives the package",
            c.name
        );
    }
}

// ---------------------------------------------------------------------------
// H.5: scalar workbooks are byte-identical before/after the spill work
// ---------------------------------------------------------------------------

/// FNV-1a 64: a tiny, stable hash independent of std's randomized hashers, so
/// the golden constant below is comparable across builds and machines.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

/// Golden hash of the scalar workbook below, frozen from the pre-Lane-H
/// writer. The spill persistence must not change a byte of a workbook that has
/// no array formulas.
const GOLDEN_SCALAR_HASH: u64 = 0xc3b2_eb40_b43f_05aa;

fn scalar_book() -> Workbook {
    let mut wb = Workbook::new();
    wb.sheets[0].name = "Scalar".into();
    let mut rows: Vec<Row> = Vec::new();
    push_cell(&mut rows, 1, 1, CellValue::Number(2.0));
    push_cell(&mut rows, 1, 2, CellValue::Number(3.0));
    push_cell(&mut rows, 1, 3, CellValue::Str("hello".into()));
    push_cell(&mut rows, 1, 4, CellValue::Bool(true));
    push_cell(&mut rows, 1, 5, CellValue::Error("#N/A".into()));
    push_cell(
        &mut rows,
        2,
        1,
        CellValue::Formula {
            text: "=A1+B1".into(),
            kind: FormulaKind::Normal,
            cached: None,
        },
    );
    push_cell(
        &mut rows,
        2,
        2,
        CellValue::Formula {
            text: "=IF(A1>0,\"yes\",\"no\")".into(),
            kind: FormulaKind::Normal,
            cached: None,
        },
    );
    push_cell(
        &mut rows,
        2,
        3,
        CellValue::Formula {
            text: "=A1=2".into(),
            kind: FormulaKind::Normal,
            cached: None,
        },
    );
    push_cell(
        &mut rows,
        2,
        4,
        CellValue::Formula {
            text: "=SUM(A1:B1)".into(),
            kind: FormulaKind::Normal,
            cached: None,
        },
    );
    push_cell(
        &mut rows,
        3,
        1,
        CellValue::Formula {
            text: "=C1&\"!\"".into(),
            kind: FormulaKind::Normal,
            cached: None,
        },
    );
    rows.sort_by_key(|r| r.row);
    wb.sheets[0].rows = rows;
    wb
}

#[test]
fn scalar_workbook_output_is_byte_identical_to_the_lane_h_baseline() {
    let mut wb = scalar_book();
    let opts = CalcOptions {
        force_recalc: true,
        ..Default::default()
    };
    let report = hydrate_workbook(&mut wb, &opts);
    assert_eq!(report.fallback, 0, "{report:?}");
    assert_eq!(report.computed, 5, "{report:?}");

    let bytes = write_workbook_bytes(&wb).expect("scalar workbook serializes");
    let first = fnv1a(&bytes);
    // determinism: a second serialization of the same model hashes identically
    let again = fnv1a(&write_workbook_bytes(&wb).expect("serializes again"));
    assert_eq!(first, again, "writer output must be deterministic");

    assert_eq!(
        first, GOLDEN_SCALAR_HASH,
        "scalar output changed vs the pre-Lane-H baseline (hash {first:#x})"
    );
}
