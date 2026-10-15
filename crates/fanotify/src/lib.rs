//! fanotify for osreflog: one filesystem-wide mark that reports directory file handles and
//! entry names, a safe parser for the events, and `open_by_handle_at` to resolve them.

pub mod event;
#[allow(unsafe_code)]
pub mod sys;

pub use event::{Event, Fid, Handle, parse};
pub use sys::{Group, fsid_of, open_by_handle};

/// The events osreflog watches: entries created, deleted or renamed (old and new name in one
/// event), attribute changes, and files closed after writing; for directories too.
pub const MASK: u64 = event::FAN_CREATE
    | event::FAN_DELETE
    | event::FAN_RENAME
    | event::FAN_ATTRIB
    | event::FAN_CLOSE_WRITE
    | event::FAN_ONDIR;
