//! Image round-trip (T1-2b): write one image per anchor kind, read back via the
//! turbo reader, and assert bytes + anchor coordinates survive.
#![cfg(feature = "__arrow")]

use pretty_assertions::assert_eq;
use std::sync::Arc;

use kyrax::turbo::write::{Anchor, Image, ImageFormat, Workbook, cm_to_emu, write_workbook_bytes};
use kyrax::turbo::{Features, ReadImageAnchor, read_workbook_turbo};

const TEST_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x01, 0x02, 0x03,
];
const TEST_JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46];

fn tmp_xlsx(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("nextexcel_images_roundtrip_{name}.xlsx"));
    p
}

fn build_workbook() -> Workbook {
    let mut wb = Workbook::with_sheet("Data");
    let sheet = &mut wb.sheets[0];
    sheet.images.push(Image {
        bytes: Arc::from(TEST_PNG),
        format: ImageFormat::Png,
        anchor: Anchor::OneCell {
            cell: "B2".into(),
            col_off: 76200,
            row_off: 50800,
            width_cm: 4.0,
            height_cm: 3.0,
        },
    });
    sheet.images.push(Image {
        bytes: Arc::from(TEST_JPEG),
        format: ImageFormat::Jpeg,
        anchor: Anchor::TwoCell {
            from_cell: "C3".into(),
            from_off: (1000, 2000),
            to_cell: "F6".into(),
            to_off: (3000, 4000),
            edit_as: Some("oneCell".into()),
        },
    });
    sheet.images.push(Image {
        bytes: Arc::from(TEST_PNG),
        format: ImageFormat::Png,
        anchor: Anchor::Absolute {
            x_emu: 1_000_000,
            y_emu: 2_000_000,
            cx_emu: 3_000_000,
            cy_emu: 4_000_000,
        },
    });
    wb
}

#[test]
fn images_roundtrip_all_three_anchor_kinds() {
    let wb = build_workbook();
    let bytes = write_workbook_bytes(&wb).expect("write");
    let path = tmp_xlsx("three_anchors");
    std::fs::write(&path, &bytes).expect("write temp file");

    let read = read_workbook_turbo(path.to_str().unwrap(), Features::IMAGES)
        .expect("read back with images flag");
    let sheet = &read.sheets[0];
    let imgs = sheet.images.as_ref().expect("images flag on");

    assert_eq!(imgs.len(), 3, "all three images must read back");

    // 1) oneCell PNG
    assert_eq!(imgs[0].bytes.as_ref(), TEST_PNG);
    assert_eq!(imgs[0].anchor.kind_str(), "oneCell");
    match &imgs[0].anchor {
        ReadImageAnchor::OneCell { from, cx, cy } => {
            assert_eq!((from.col, from.row), (1, 1), "B2 -> 0-based 1,1");
            assert_eq!((from.col_off, from.row_off), (76200, 50800));
            assert_eq!((*cx, *cy), (cm_to_emu(4.0), cm_to_emu(3.0)),);
        }
        other => panic!("expected oneCell anchor, got {other:?}"),
    }

    // 2) twoCell JPEG
    assert_eq!(imgs[1].bytes.as_ref(), TEST_JPEG);
    assert_eq!(imgs[1].anchor.kind_str(), "twoCell");
    match &imgs[1].anchor {
        ReadImageAnchor::TwoCell { from, to, edit_as } => {
            assert_eq!((from.col, from.row), (2, 2), "C3 -> 0-based 2,2");
            assert_eq!((from.col_off, from.row_off), (1000, 2000));
            assert_eq!((to.col, to.row), (5, 5), "F6 -> 0-based 5,5");
            assert_eq!((to.col_off, to.row_off), (3000, 4000));
            assert_eq!(edit_as.as_deref(), Some("oneCell"));
        }
        other => panic!("expected twoCell anchor, got {other:?}"),
    }

    // 3) absolute PNG (same bytes as image 1 — dedup must still yield a pic)
    assert_eq!(imgs[2].bytes.as_ref(), TEST_PNG);
    assert_eq!(imgs[2].anchor.kind_str(), "absolute");
    match &imgs[2].anchor {
        ReadImageAnchor::Absolute { x, y, cx, cy } => {
            assert_eq!(
                (*x, *y, *cx, *cy),
                (1_000_000, 2_000_000, 3_000_000, 4_000_000)
            );
        }
        other => panic!("expected absolute anchor, got {other:?}"),
    }
}

/// Content-addressed dedup: two identical PNG blobs at DIFFERENT anchors must
/// collapse to ONE part under xl/media/ while keeping TWO anchor entries in the
/// drawing, both rels pointing at the same media target.
#[test]
fn images_dedup_identical_bytes_one_media_part_two_pics() {
    let mut wb = Workbook::with_sheet("Data");
    let sheet = &mut wb.sheets[0];
    sheet.images.push(Image {
        bytes: Arc::from(TEST_PNG),
        format: ImageFormat::Png,
        anchor: Anchor::OneCell {
            cell: "B2".into(),
            col_off: 76200,
            row_off: 50800,
            width_cm: 4.0,
            height_cm: 3.0,
        },
    });
    sheet.images.push(Image {
        bytes: Arc::from(TEST_PNG),
        format: ImageFormat::Png,
        anchor: Anchor::TwoCell {
            from_cell: "C3".into(),
            from_off: (1000, 2000),
            to_cell: "F6".into(),
            to_off: (3000, 4000),
            edit_as: None,
        },
    });
    let bytes = write_workbook_bytes(&wb).expect("write");

    // Exactly one media part; the second blob must be deduped into image1.png.
    let png = kyrax::turbo::read_entry(&bytes, "xl/media/image1.png")
        .expect("read_entry ok")
        .expect("image1.png present");
    assert_eq!(png, TEST_PNG);
    assert!(
        kyrax::turbo::read_entry(&bytes, "xl/media/image2.png")
            .expect("read_entry ok")
            .is_none(),
        "identical bytes must share one media part"
    );

    // Two anchor entries in the drawing, each with a blip.
    let drawing = kyrax::turbo::read_entry(&bytes, "xl/drawings/drawing1.xml")
        .expect("read_entry ok")
        .expect("drawing1.xml present");
    let drawing = String::from_utf8(drawing).expect("utf8 drawing");
    assert_eq!(drawing.matches("<pic>").count(), 2, "{drawing}");
    assert_eq!(
        drawing.matches(r#"<a:blip r:embed="#).count(),
        2,
        "every pic needs a blip: {drawing}"
    );

    // Both rels point at the same media target.
    let drels = kyrax::turbo::read_entry(&bytes, "xl/drawings/_rels/drawing1.xml.rels")
        .expect("read_entry ok")
        .expect("drawing rels present");
    let drels = String::from_utf8(drels).expect("utf8 rels");
    let target = r#"Target="../media/image1.png""#;
    assert_eq!(
        drels.matches(target).count(),
        2,
        "both pics must reference the shared media part: {drels}"
    );
}
