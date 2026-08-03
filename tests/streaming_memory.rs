//! Does streaming actually bound memory? — the north star's RSS lever.
//!
//! `plans/northstar_metric.md` has two levers: CPU-seconds/file, and PEAK RSS
//! PER WORKER, which sets how many workers fit in a box and must stay under
//! 2 GB. Every other unit in this project moved the CPU lever. Streaming is the
//! one that moves RSS, and the claim it rests on is that peak allocation is
//! O(window) rather than O(sheet).
//!
//! That claim is worthless asserted. This file measures it.
//!
//! METHOD. A counting global allocator wraps the system allocator and tracks
//! live-bytes highwater. That is an EXACT allocation highwater, not a sampled
//! RSS reading — sampling can miss a spike entirely, and on Windows there is no
//! `/proc` to sample from anyway.
//!
//! WHAT IS ASSERTED. Ratios and scaling, never absolute bytes: absolute figures
//! are machine- and allocator-specific and would make this a flaky gate. The
//! shape being pinned is that eager peak grows with the sheet while streaming
//! peak does not.
//!
//!     cargo test --release --features __arrow --test streaming_memory -- --nocapture

#![cfg(feature = "__arrow")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use kyrax::turbo::{Features, SheetStream, StreamOptions, read_workbook_turbo_sheet};

// ---------------------------------------------------------------------------
// counting allocator
// ---------------------------------------------------------------------------

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            let live = LIVE.fetch_add(l.size(), Ordering::Relaxed) + l.size();
            // Monotonic max. Racy across threads by a bounded amount, which is
            // fine: we compare magnitudes, not exact bytes.
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            if new >= l.size() {
                let live = LIVE.fetch_add(new - l.size(), Ordering::Relaxed) + (new - l.size());
                PEAK.fetch_max(live, Ordering::Relaxed);
            } else {
                LIVE.fetch_sub(l.size() - new, Ordering::Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static A: Counting = Counting;

/// Run `f`, returning its value and the allocation highwater reached during it.
fn measure<T>(f: impl FnOnce() -> T) -> (T, usize) {
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
    let out = f();
    let peak = PEAK.load(Ordering::Relaxed);
    let base = LIVE.load(Ordering::Relaxed);
    (out, peak.saturating_sub(base))
}

// ---------------------------------------------------------------------------
// fixture
// ---------------------------------------------------------------------------

/// Build an xlsx with `rows` x 8 cells into the system temp dir (never the repo).
/// Mixed numeric and string columns, because homogeneous columns take a
/// different path in the reader — see PERF_EXPERIMENTS_PHASE2.md section P0.
fn fixture(rows: usize) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("kyrax_streammem_{rows}.xlsx"));
    if p.exists() {
        return p;
    }

    let mut sheet = String::with_capacity(rows * 240);
    sheet.push_str(
        "<?xml version=\"1.0\"?><worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData>",
    );
    for r in 1..=rows {
        sheet.push_str(&format!("<row r=\"{r}\">"));
        for c in 0..8u32 {
            let col = (b'A' + c as u8) as char;
            if c % 3 == 0 {
                sheet.push_str(&format!(
                    "<c r=\"{col}{r}\" t=\"inlineStr\"><is><t>row{r}col{c}</t></is></c>"
                ));
            } else {
                sheet.push_str(&format!("<c r=\"{col}{r}\"><v>{}.{}</v></c>", r, c));
            }
        }
        sheet.push_str("</row>");
    }
    sheet.push_str("</sheetData></worksheet>");

    let ct = "<?xml version=\"1.0\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
        <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
        <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
        <Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>\
        <Override PartName=\"/xl/worksheets/sheet1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/></Types>";
    let root = "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>";
    let wb = "<?xml version=\"1.0\"?><workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
        <sheets><sheet name=\"S\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>";
    let wbr = "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/></Relationships>";

    let parts: [(&str, &[u8]); 5] = [
        ("[Content_Types].xml", ct.as_bytes()),
        ("_rels/.rels", root.as_bytes()),
        ("xl/workbook.xml", wb.as_bytes()),
        ("xl/_rels/workbook.xml.rels", wbr.as_bytes()),
        ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
    ];
    std::fs::write(&p, store_zip(&parts)).expect("write fixture");
    p
}

/// Minimal STORE-only zip writer. The crate's own ZipWriter is not public, and
/// an integration test may only use the public API — so the fixture builder is
/// self-contained rather than reaching into private modules.
fn store_zip(parts: &[(&str, &[u8])]) -> Vec<u8> {
    fn crc32(b: &[u8]) -> u32 {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *e = c;
        }
        let mut c = 0xFFFF_FFFFu32;
        for &x in b {
            c = t[((c ^ x as u32) & 0xFF) as usize] ^ (c >> 8);
        }
        c ^ 0xFFFF_FFFF
    }
    let mut out: Vec<u8> = Vec::new();
    let mut cd: Vec<u8> = Vec::new();
    let mut n = 0u16;
    for (name, data) in parts {
        let off = out.len() as u32;
        let crc = crc32(data);
        let nb = name.as_bytes();
        out.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04]);
        out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(nb.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(nb);
        out.extend_from_slice(data);

        cd.extend_from_slice(&[0x50, 0x4B, 0x01, 0x02]);
        cd.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        cd.extend_from_slice(&crc.to_le_bytes());
        cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(nb.len() as u16).to_le_bytes());
        cd.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        cd.extend_from_slice(&off.to_le_bytes());
        cd.extend_from_slice(nb);
        n += 1;
    }
    let cd_off = out.len() as u32;
    let cd_len = cd.len() as u32;
    out.extend_from_slice(&cd);
    out.extend_from_slice(&[0x50, 0x4B, 0x05, 0x06, 0, 0, 0, 0]);
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&cd_len.to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

fn stream_peak(path: &str) -> (usize, usize) {
    let opts = StreamOptions::default();
    measure(|| {
        let mut s = SheetStream::open(path, 0, opts.clone()).expect("open stream");
        let mut n = 0usize;
        // Drop each batch as we go — a caller that collects everything is
        // asking for O(sheet) by definition, so that is not what we measure.
        while let Some(b) = s.next_batch(&opts).expect("batch") {
            n += b.num_rows();
        }
        n
    })
}

fn eager_peak(path: &str) -> (usize, usize) {
    measure(|| {
        let sh = read_workbook_turbo_sheet(path, Features::VALUES, 0).expect("eager read");
        sh.sheets.first().map(|s| s.nrows).unwrap_or(0)
    })
}

// ---------------------------------------------------------------------------

/// The load-bearing test: eager peak scales with the sheet, streaming peak does not.
#[test]
fn streaming_peak_is_bounded_while_eager_grows() {
    let small_p = fixture(20_000);
    let large_p = fixture(200_000);
    let small = small_p.to_str().unwrap();
    let large = large_p.to_str().unwrap();

    let (rs_small, ep_small) = eager_peak(small);
    let (rs_large, ep_large) = eager_peak(large);
    let (ss_small, sp_small) = stream_peak(small);
    let (ss_large, sp_large) = stream_peak(large);

    println!("rows read  eager {rs_small}/{rs_large}  streaming {ss_small}/{ss_large}");
    println!(
        "eager     peak: {:>9.2} MB (20k) -> {:>9.2} MB (200k)  = {:.1}x for 10x rows",
        ep_small as f64 / 1e6,
        ep_large as f64 / 1e6,
        ep_large as f64 / ep_small.max(1) as f64
    );
    println!(
        "streaming peak: {:>9.2} MB (20k) -> {:>9.2} MB (200k)  = {:.1}x for 10x rows",
        sp_small as f64 / 1e6,
        sp_large as f64 / 1e6,
        sp_large as f64 / sp_small.max(1) as f64
    );
    println!(
        "streaming uses {:.1}x less memory than eager at 200k rows",
        ep_large as f64 / sp_large.max(1) as f64
    );

    // Both modes must actually have read the data — a stream that silently
    // yields nothing would otherwise "win" on memory.
    assert!(
        ss_large >= rs_large,
        "streaming dropped rows: {ss_large} < {rs_large}"
    );

    let eager_growth = ep_large as f64 / ep_small.max(1) as f64;
    let stream_growth = sp_large as f64 / sp_small.max(1) as f64;

    // The shape that matters: 10x the rows must not cost 10x the memory when
    // streaming, while eager is expected to grow roughly linearly.
    assert!(
        stream_growth < 3.0,
        "streaming peak grew {stream_growth:.1}x for 10x rows — it is not O(window)"
    );
    assert!(
        stream_growth < eager_growth,
        "streaming peak grew as fast as eager ({stream_growth:.1}x vs {eager_growth:.1}x) — \
         streaming is buying nothing"
    );
    assert!(
        (ep_large as f64) > (sp_large as f64) * 2.0,
        "streaming peak is not materially below eager at 200k rows: \
         {:.2} MB vs {:.2} MB",
        sp_large as f64 / 1e6,
        ep_large as f64 / 1e6
    );
}

/// Bytes held per cell — the portable number. Absolute MB are machine-specific;
/// this is what transfers to another box and another architecture.
#[test]
fn streaming_bytes_per_cell_is_reported() {
    let large_p = fixture(200_000);
    let large = large_p.to_str().unwrap();
    let cells = 200_000f64 * 8.0;

    let (_, ep) = eager_peak(large);
    let (_, sp) = stream_peak(large);

    println!(
        "200k x 8 = {cells:.0} cells — eager {:.1} B/cell, streaming {:.1} B/cell",
        ep as f64 / cells,
        sp as f64 / cells
    );

    // A worker budget of 2 GB at 10M cells needs to stay well under 200 B/cell.
    let per_cell = sp as f64 / cells;
    assert!(
        per_cell < 200.0,
        "streaming holds {per_cell:.1} B/cell — a 10M-cell sheet would not fit a 2 GB worker"
    );
}
