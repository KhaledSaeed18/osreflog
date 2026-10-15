//! A real send stream from the loopback integration test (scripts/p2/it.sh), committed so the
//! parser is checked against kernel output without root. Tree changes between the two
//! snapshots: edit.txt rewritten, gone.txt deleted, new.txt created, dir renamed to moved,
//! link created as a symlink.
#![allow(clippy::unwrap_used)]

use osreflog_btrfs::{Kind, Op, diff_stream};

#[test]
fn small_stream() {
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/small.nodata.stream"
    ))
    .unwrap();
    let diff = diff_stream(&bytes[..]).unwrap();
    assert_eq!(diff.version, 2);
    let got: Vec<(String, Op, Kind)> = diff
        .changes
        .into_iter()
        .map(|c| (c.path, c.op, c.kind))
        .collect();
    assert_eq!(
        got,
        [
            ("/fixture-tree/edit.txt".to_owned(), Op::Modify, Kind::File),
            (
                "/fixture-tree/gone.txt".to_owned(),
                Op::Delete,
                Kind::Unknown
            ),
            ("/fixture-tree/link".to_owned(), Op::Create, Kind::Symlink),
            (
                "/fixture-tree/moved".to_owned(),
                Op::Rename {
                    from: "/fixture-tree/dir".to_owned()
                },
                Kind::Unknown
            ),
            ("/fixture-tree/new.txt".to_owned(), Op::Create, Kind::File),
        ]
    );
}
