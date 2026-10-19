//! btrfs for osreflog: the send-stream parser and change sets (safe, testable without root)
//! and the ioctls behind checkpoints (root, real filesystems only).

pub mod changes;
pub mod crc32c;
#[allow(unsafe_code)]
pub mod ioctl;
pub mod stream;

use std::io::Read;

pub use changes::{Change, ChangeSetBuilder, Kind, Op, Release, StreamInfo, flags, is_orphan_name};
pub use stream::{Command, StreamError, StreamReader};

/// A parsed send stream: the change set, what the stream said about itself, and its protocol
/// version.
#[derive(Debug)]
pub struct Diff {
    pub changes: Vec<Change>,
    pub info: StreamInfo,
    pub version: u32,
    /// Sum of content bytes over all changes (written, cloned or re-extented).
    pub changed_bytes: u64,
}

/// Reads a whole send stream into a change set.
///
/// # Errors
/// Whatever the reader or the parser reports.
pub fn diff_stream<R: Read>(reader: R) -> Result<Diff, StreamError> {
    let mut stream = StreamReader::new(reader)?;
    let version = stream.version();
    let mut builder = ChangeSetBuilder::new();
    while let Some(command) = stream.next_command()? {
        builder.apply(&command)?;
    }
    let (changes, info) = builder.finish();
    let changed_bytes = changes.iter().map(|c| c.bytes).sum();
    Ok(Diff {
        changes,
        info,
        version,
        changed_bytes,
    })
}
