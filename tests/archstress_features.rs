//! A2 Wave-1 Rust invariants that are not usefully observable through Python.
//!
//! This integration target deliberately uses only public `kyrax` APIs. Package-level
//! lifecycle coverage lives in `tests_turbo/architecture_stress/test_features.py`.

use kyrax::turbo::features::threaded_comments::{
    Person, ThreadedComment, parse_persons, parse_threaded_comments, write_persons,
    write_threaded_comments,
};
use kyrax::turbo::write::{ImageFormat, detect_image_format};

#[test]
fn a2_image_magic_detection_distinguishes_formats() {
    let png = b"\x89PNG\r\n\x1a\nrest";
    let jpeg = b"\xff\xd8\xff\xe0JFIFrest";
    let gif = b"GIF89arest";

    assert_eq!(detect_image_format(png), Some(ImageFormat::Png));
    assert_eq!(detect_image_format(jpeg), Some(ImageFormat::Jpeg));
    assert_eq!(detect_image_format(gif), Some(ImageFormat::Gif));
    assert_eq!(detect_image_format(b"not-an-image"), None);
}

#[test]
fn a2_threaded_emitters_are_deterministic_and_round_trip() {
    let persons = vec![
        Person {
            id: "{alice}".into(),
            display_name: "Alice & Co".into(),
        },
        Person {
            id: "{bob}".into(),
            display_name: "Bob <B>".into(),
        },
    ];
    let comments = vec![
        ThreadedComment {
            cell: "A1".into(),
            text: "Unicode: नमस्ते 😀 & <ok>".into(),
            author_id: "{alice}".into(),
            created: Some("2024-01-01T00:00:00Z".into()),
            id: "{c1}".into(),
            parent_id: None,
        },
        ThreadedComment {
            cell: "A1".into(),
            text: "reply".into(),
            author_id: "{bob}".into(),
            created: None,
            id: "{c2}".into(),
            parent_id: Some("{c1}".into()),
        },
    ];

    let threaded_xml = write_threaded_comments(&comments);
    let persons_xml = write_persons(&persons);
    assert_eq!(threaded_xml, write_threaded_comments(&comments));
    assert_eq!(persons_xml, write_persons(&persons));
    assert_eq!(parse_threaded_comments(&threaded_xml).unwrap(), comments);
    assert_eq!(parse_persons(&persons_xml).unwrap(), persons);
}

#[test]
fn a2_threaded_emitter_is_not_workbook_packaging() {
    let xml = write_threaded_comments(&[]);
    assert!(xml.starts_with(b"<?xml"));
    assert!(
        xml.windows(b"<ThreadedComments".len())
            .any(|w| w == b"<ThreadedComments")
    );
    // This target proves only the standalone emitter. Public workbook packaging is
    // intentionally recorded as KNOWN-GAP in NOTES/A2.md and the Python suite.
}
