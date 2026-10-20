//! Turns a send stream into a change set: what changed between two snapshots, keyed by the
//! path each object has at the end of the interval.
//!
//! The stream replays edits on the parent in order, using current paths. New objects are
//! created under orphan names (`o<ino>-<gen>-0`) and renamed into place; existing objects can
//! be moved to orphan names and back; directory renames move everything below them. So the
//! builder tracks every touched object by its current path, remembers where pre-existing
//! objects came from, and re-keys descendants when a directory moves.

use std::collections::{BTreeMap, HashMap};

use crate::stream::{Command, StreamError, attr, cmd, path_text};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    Special,
    Unknown,
}

impl Kind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::File => "file",
            Kind::Dir => "dir",
            Kind::Symlink => "symlink",
            Kind::Special => "special",
            Kind::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Op {
    Create,
    Modify,
    Delete,
    /// Moved from `from` (and possibly modified: see the flags).
    Rename {
        from: String,
    },
}

/// What about an object changed, as bits.
pub mod flags {
    pub const CONTENT: u32 = 1;
    pub const MODE: u32 = 1 << 1;
    pub const OWNER: u32 = 1 << 2;
    pub const XATTR: u32 = 1 << 3;
    pub const TIMES: u32 = 1 << 4;
    pub const FILEATTR: u32 = 1 << 5;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Path relative to the subvolume root, starting with `/`.
    pub path: String,
    pub op: Op,
    pub kind: Kind,
    pub flags: u32,
    /// Bytes of file content written, cloned or re-extented in this interval.
    pub bytes: u64,
    /// What the interval took away from the object as it was before (for space accounting).
    pub release: Release,
}

/// What an interval took away from a pre-existing file: the content its older snapshot now
/// holds alone. Paths are where the object was before the interval, starting with `/`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Release {
    /// Nothing of its old content: a new object, or only metadata changed.
    #[default]
    Nothing,
    /// All of it: deleted, or replaced by a new file renamed over it.
    Whole { from: String },
    /// The byte ranges written over, and the size it was cut to (written in place). With too
    /// many ranges to keep, `written` has the bytes written instead.
    Part {
        from: String,
        ranges: Vec<(u64, u64)>,
        written: Option<u64>,
        truncated_to: Option<u64>,
    },
}

impl Release {
    /// The old object's path, if the interval released any of it.
    #[must_use]
    pub fn from(&self) -> Option<&str> {
        match self {
            Release::Nothing => None,
            Release::Whole { from } | Release::Part { from, .. } => Some(from),
        }
    }

    /// Bytes released, given the old file's size and the bytes it had allocated on disk.
    /// Writes past the old end (appends) release nothing; a truncation releases the tail.
    #[must_use]
    pub fn bytes(&self, old_size: u64, old_allocated: u64) -> u64 {
        let released = match self {
            Release::Nothing => 0,
            Release::Whole { .. } => return old_allocated,
            Release::Part {
                ranges,
                written,
                truncated_to,
                ..
            } => {
                let keep = truncated_to.map_or(old_size, |t| t.min(old_size));
                if let &Some(written) = written {
                    return (written.min(keep) + (old_size - keep)).min(old_allocated);
                }
                // Whole blocks, without counting a block twice where two ranges share it.
                let mut over = 0u64;
                let mut counted_to = 0u64;
                for &(start, end) in ranges {
                    let end = end.min(keep);
                    if end <= start {
                        continue;
                    }
                    let first = (start / BLOCK * BLOCK).max(counted_to);
                    let last = end.div_ceil(BLOCK) * BLOCK;
                    over += last.saturating_sub(first);
                    counted_to = counted_to.max(last);
                }
                over.min(keep.div_ceil(BLOCK) * BLOCK) + (old_size - keep)
            }
        };
        released.min(old_allocated)
    }
}

/// Ranges kept per file (64 KiB at most). Past this the ranges are dropped and the bytes
/// written are counted instead, which can't exceed what was written. Merging them into one
/// span, as at first, made a 40 MiB journal file with a few KiB written all over it count as
/// 40 MiB released, every interval.
const MAX_RANGES: usize = 4096;

/// btrfs copies whole blocks on write: a range released is counted in blocks of this size.
const BLOCK: u64 = 4096;

/// Adds `[start, end)` to sorted, disjoint ranges, merging what touches.
fn add_range(ranges: &mut Vec<(u64, u64)>, start: u64, end: u64) {
    if end <= start {
        return;
    }
    let (mut start, mut end) = (start, end);
    ranges.retain(|&(a, b)| {
        if b < start || a > end {
            true
        } else {
            start = start.min(a);
            end = end.max(b);
            false
        }
    });
    let at = ranges.partition_point(|&(a, _)| a < start);
    ranges.insert(at, (start, end));
}

/// What the stream says about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamInfo {
    pub snapshot_name: String,
    pub uuid: Vec<u8>,
    pub ctransid: u64,
    pub parent_uuid: Option<Vec<u8>>,
    pub parent_ctransid: Option<u64>,
    pub commands: u64,
}

#[derive(Debug, Clone)]
struct Node {
    /// Where a pre-existing object was before the interval; `None` for new objects.
    origin: Option<String>,
    kind: Kind,
    flags: u32,
    bytes: u64,
    /// Byte ranges written in place, and the smallest size it was truncated to.
    ranges: Vec<(u64, u64)>,
    /// More than [`MAX_RANGES`]: `ranges` dropped, the bytes written count instead.
    too_many_ranges: bool,
    truncated_to: Option<u64>,
}

#[derive(Debug, Default)]
pub struct ChangeSetBuilder {
    /// Touched objects by current path (no leading slash).
    nodes: BTreeMap<String, Node>,
    /// Pre-existing objects that are gone: original path → kind.
    deleted: HashMap<String, Kind>,
    info: StreamInfo,
}

impl ChangeSetBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one command.
    ///
    /// # Errors
    /// If the command lacks an attribute its type requires.
    pub fn apply(&mut self, c: &Command) -> Result<(), StreamError> {
        self.info.commands += 1;
        match c.cmd {
            cmd::SUBVOL | cmd::SNAPSHOT => {
                self.info.snapshot_name = path_text(c.need(attr::PATH)?);
                self.info.uuid = c.get(attr::UUID).unwrap_or_default().to_vec();
                self.info.ctransid = c.u64(attr::CTRANSID).unwrap_or(0);
                self.info.parent_uuid = c.get(attr::CLONE_UUID).map(<[u8]>::to_vec);
                self.info.parent_ctransid = c.u64(attr::CLONE_CTRANSID);
            }
            // LINK adds a new name for an existing file: a new path, so a create.
            cmd::MKFILE | cmd::LINK => self.create(c, Kind::File)?,
            cmd::MKDIR => self.create(c, Kind::Dir)?,
            cmd::SYMLINK => self.create(c, Kind::Symlink)?,
            cmd::MKNOD | cmd::MKFIFO | cmd::MKSOCK => self.create(c, Kind::Special)?,
            cmd::RENAME => {
                let from = path_text(c.need(attr::PATH)?);
                let to = path_text(c.need(attr::PATH_TO)?);
                self.rename(&from, &to);
            }
            cmd::UNLINK => self.remove(&path_text(c.need(attr::PATH)?), Kind::Unknown),
            cmd::RMDIR => self.remove(&path_text(c.need(attr::PATH)?), Kind::Dir),
            cmd::WRITE | cmd::ENCODED_WRITE => {
                let len = c.get(attr::DATA).map_or(0, <[u8]>::len) as u64;
                let len = c.u64(attr::UNENCODED_LEN).unwrap_or(len);
                self.content(c, len)?;
            }
            cmd::CLONE => {
                let len = c.u64(attr::CLONE_LEN).unwrap_or(0);
                self.content(c, len)?;
            }
            cmd::UPDATE_EXTENT => {
                let len = c.u64(attr::SIZE).unwrap_or(0);
                self.content(c, len)?;
            }
            cmd::TRUNCATE => {
                self.content(c, 0)?;
                let path = path_text(c.need(attr::PATH)?);
                if let (Some(size), Some(node)) = (c.u64(attr::SIZE), self.nodes.get_mut(&path)) {
                    node.truncated_to = Some(node.truncated_to.map_or(size, |t| t.min(size)));
                }
            }
            cmd::FALLOCATE => self.content(c, 0)?,
            cmd::CHMOD => self.attr(c, flags::MODE)?,
            cmd::CHOWN => self.attr(c, flags::OWNER)?,
            cmd::UTIMES => self.attr(c, flags::TIMES)?,
            cmd::SET_XATTR | cmd::REMOVE_XATTR => self.attr(c, flags::XATTR)?,
            cmd::FILEATTR => self.attr(c, flags::FILEATTR)?,
            // END, ENABLE_VERITY and anything unknown change nothing we report.
            _ => {}
        }
        Ok(())
    }

    fn create(&mut self, c: &Command, kind: Kind) -> Result<(), StreamError> {
        let path = path_text(c.need(attr::PATH)?);
        self.nodes.insert(
            path,
            Node {
                origin: None,
                kind,
                flags: 0,
                bytes: 0,
                ranges: Vec::new(),
                too_many_ranges: false,
                truncated_to: None,
            },
        );
        Ok(())
    }

    /// Where the object now at `path` was before the interval; `None` if it is new.
    fn origin_of(&self, path: &str) -> Option<String> {
        if let Some(node) = self.nodes.get(path) {
            return node.origin.clone();
        }
        // The nearest touched ancestor decides: a moved directory carries its children.
        let mut cut = path.len();
        while let Some(slash) = path[..cut].rfind('/') {
            cut = slash;
            if let Some(node) = self.nodes.get(&path[..cut]) {
                return node.origin.as_ref().map(|o| format!("{o}{}", &path[cut..]));
            }
        }
        Some(path.to_owned())
    }

    fn touch(&mut self, path: &str) -> &mut Node {
        if !self.nodes.contains_key(path) {
            let origin = self.origin_of(path);
            self.nodes.insert(
                path.to_owned(),
                Node {
                    origin,
                    kind: Kind::Unknown,
                    flags: 0,
                    bytes: 0,
                    ranges: Vec::new(),
                    too_many_ranges: false,
                    truncated_to: None,
                },
            );
        }
        // Just inserted or already present.
        self.nodes.get_mut(path).unwrap_or_else(|| unreachable!())
    }

    fn content(&mut self, c: &Command, len: u64) -> Result<(), StreamError> {
        let path = path_text(c.need(attr::PATH)?);
        let offset = c.u64(attr::FILE_OFFSET);
        let node = self.touch(&path);
        node.flags |= flags::CONTENT;
        node.bytes = node.bytes.saturating_add(len);
        if let Some(offset) = offset
            && !node.too_many_ranges
        {
            add_range(&mut node.ranges, offset, offset.saturating_add(len));
            if node.ranges.len() > MAX_RANGES {
                node.ranges = Vec::new();
                node.too_many_ranges = true;
            }
        }
        if node.kind == Kind::Unknown {
            node.kind = Kind::File;
        }
        Ok(())
    }

    fn attr(&mut self, c: &Command, flag: u32) -> Result<(), StreamError> {
        let path = path_text(c.need(attr::PATH)?);
        self.touch(&path).flags |= flag;
        Ok(())
    }

    fn rename(&mut self, from: &str, to: &str) {
        let node = match self.nodes.remove(from) {
            Some(node) => node,
            None => Node {
                origin: self.origin_of(from),
                kind: Kind::Unknown,
                flags: 0,
                bytes: 0,
                ranges: Vec::new(),
                too_many_ranges: false,
                truncated_to: None,
            },
        };
        // Re-key touched descendants of a moved directory.
        let prefix = format!("{from}/");
        let moved: Vec<String> = self
            .nodes
            .range(prefix.clone()..)
            .take_while(|(k, _)| k.starts_with(&prefix))
            .map(|(k, _)| k.clone())
            .collect();
        for old in moved {
            if let Some(child) = self.nodes.remove(&old) {
                self.nodes
                    .insert(format!("{to}/{}", &old[prefix.len()..]), child);
            }
        }
        self.nodes.insert(to.to_owned(), node);
    }

    fn remove(&mut self, path: &str, kind: Kind) {
        let (origin, kind) = match self.nodes.remove(path) {
            Some(node) => (
                node.origin,
                if node.kind == Kind::Unknown {
                    kind
                } else {
                    node.kind
                },
            ),
            None => (self.origin_of(path), kind),
        };
        // New objects that die in the same interval leave no trace.
        if let Some(origin) = origin {
            self.deleted.insert(origin, kind);
        }
    }

    /// The final change set, sorted by path. Directories whose only change is their times
    /// are dropped when something inside them changed: that is what moved their mtime.
    #[must_use]
    pub fn finish(self) -> (Vec<Change>, StreamInfo) {
        let mut out = Vec::with_capacity(self.nodes.len() + self.deleted.len());
        let mut parents = std::collections::HashSet::new();
        let mut note_parents = |p: &str| {
            let mut cut = p.len();
            while let Some(slash) = p[..cut].rfind('/') {
                cut = slash;
                if !parents.insert(p[..cut].to_owned()) {
                    break;
                }
            }
            parents.insert(String::new());
        };
        for (path, node) in &self.nodes {
            // Anything but a times-only touch of an object still at its old path is a change
            // inside the parent (a created or renamed object counts, even with only times).
            if node.flags != flags::TIMES || node.origin.as_deref() != Some(path.as_str()) {
                note_parents(path);
            }
        }
        for path in self.deleted.keys() {
            note_parents(path);
        }

        let mut deleted = self.deleted;
        for (path, node) in self.nodes {
            let mut release = match &node.origin {
                Some(origin) if node.flags & flags::CONTENT != 0 => Release::Part {
                    from: format!("/{origin}"),
                    ranges: node.ranges,
                    written: node.too_many_ranges.then_some(node.bytes),
                    truncated_to: node.truncated_to,
                },
                _ => Release::Nothing,
            };
            let op = match &node.origin {
                // A new object where an old one was deleted: replaced in place (editors,
                // rpm and most config tools write a new file and rename it over the old one).
                None if deleted.remove(&path).is_some() => {
                    release = Release::Whole {
                        from: format!("/{path}"),
                    };
                    Op::Modify
                }
                None => Op::Create,
                Some(origin) if *origin != path => Op::Rename {
                    from: format!("/{origin}"),
                },
                Some(_) => {
                    if node.flags == 0 || (node.flags == flags::TIMES && parents.contains(&path)) {
                        continue;
                    }
                    Op::Modify
                }
            };
            let flags = if op == Op::Modify && node.origin.is_none() {
                node.flags | flags::CONTENT
            } else {
                node.flags
            };
            out.push(Change {
                path: format!("/{path}"),
                op,
                kind: node.kind,
                flags,
                bytes: node.bytes,
                release,
            });
        }
        for (path, kind) in deleted {
            out.push(Change {
                release: Release::Whole {
                    from: format!("/{path}"),
                },
                path: format!("/{path}"),
                op: Op::Delete,
                kind,
                flags: 0,
                bytes: 0,
            });
        }
        out.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.op.cmp(&b.op)));
        (out, self.info)
    }
}

/// True for the temporary names the kernel uses for not-yet-placed inodes.
#[must_use]
pub fn is_orphan_name(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some(rest) = name.strip_prefix('o') else {
        return false;
    };
    let parts: Vec<&str> = rest.split('-').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::writer::StreamWriter;
    use crate::stream::{StreamReader, cmd};

    fn changes(bytes: &[u8]) -> Vec<Change> {
        let mut r = StreamReader::new(bytes).unwrap();
        let mut b = ChangeSetBuilder::new();
        while let Some(c) = r.next_command().unwrap() {
            b.apply(&c).unwrap();
        }
        b.finish().0
    }

    fn summary(cs: &[Change]) -> Vec<String> {
        cs.iter()
            .map(|c| match &c.op {
                Op::Rename { from } => format!("rename {from} -> {}", c.path),
                op => format!("{op:?} {}", c.path).to_lowercase(),
            })
            .collect()
    }

    #[test]
    fn new_file_placed_from_orphan_name() {
        let s = StreamWriter::new(2)
            .path(cmd::MKFILE, "o300-9-0")
            .rename("o300-9-0", "etc/new.conf")
            .update_extent("etc/new.conf", 0, 120)
            .path(cmd::UTIMES, "etc")
            .finish();
        let cs = changes(&s);
        assert_eq!(summary(&cs), ["create /etc/new.conf"]);
        assert_eq!(cs[0].bytes, 120);
        assert_eq!(cs[0].kind, Kind::File);
    }

    #[test]
    fn modified_and_deleted_existing_files() {
        let s = StreamWriter::new(2)
            .update_extent("etc/hosts", 0, 10)
            .path(cmd::UNLINK, "etc/old.conf")
            .path(cmd::RMDIR, "etc/gone.d")
            .path(cmd::UTIMES, "etc")
            .finish();
        assert_eq!(
            summary(&changes(&s)),
            [
                "delete /etc/gone.d",
                "modify /etc/hosts",
                "delete /etc/old.conf"
            ]
        );
    }

    #[test]
    fn existing_file_through_orphan_name_and_back_is_a_modify() {
        let s = StreamWriter::new(2)
            .rename("usr/bin/tool", "o257-3-0")
            .update_extent("o257-3-0", 0, 8)
            .rename("o257-3-0", "usr/bin/tool")
            .finish();
        assert_eq!(summary(&changes(&s)), ["modify /usr/bin/tool"]);
    }

    #[test]
    fn directory_rename_carries_children() {
        let s = StreamWriter::new(2)
            .update_extent("opt/app/config", 0, 4)
            .rename("opt/app", "opt/app2")
            .update_extent("opt/app2/data", 0, 4)
            .finish();
        let cs = changes(&s);
        assert_eq!(
            summary(&cs),
            [
                "rename /opt/app -> /opt/app2",
                "rename /opt/app/config -> /opt/app2/config",
                "rename /opt/app/data -> /opt/app2/data"
            ]
        );
    }

    #[test]
    fn transient_files_leave_no_trace() {
        let s = StreamWriter::new(2)
            .path(cmd::MKFILE, "o400-1-0")
            .update_extent("o400-1-0", 0, 4)
            .path(cmd::UNLINK, "o400-1-0")
            .finish();
        assert_eq!(changes(&s), Vec::<Change>::new());
    }

    #[test]
    fn times_only_parent_dropped_but_lone_times_change_kept() {
        let s = StreamWriter::new(2)
            .path(cmd::UTIMES, "var")
            .path(cmd::UTIMES, "var/log")
            .update_extent("var/log/messages", 0, 100)
            .path(cmd::UTIMES, "srv")
            .finish();
        assert_eq!(
            summary(&changes(&s)),
            ["modify /srv", "modify /var/log/messages"]
        );
    }

    #[test]
    fn replaced_in_place_is_a_modify() {
        // What `rpm` and editors do: write a new file, rename it over the old one.
        let s = StreamWriter::new(2)
            .path(cmd::UNLINK, "etc/hosts")
            .path(cmd::MKFILE, "o500-9-0")
            .update_extent("o500-9-0", 0, 30)
            .rename("o500-9-0", "etc/hosts")
            .finish();
        let cs = changes(&s);
        assert_eq!(summary(&cs), ["modify /etc/hosts"]);
        assert_ne!(cs[0].flags & flags::CONTENT, 0);
    }

    #[test]
    fn parent_of_a_renamed_dir_is_not_reported() {
        // The kernel sends utimes for a renamed directory and for its parent.
        let s = StreamWriter::new(2)
            .rename("opt/app", "opt/app2")
            .path(cmd::UTIMES, "opt")
            .path(cmd::UTIMES, "opt/app2")
            .finish();
        assert_eq!(summary(&changes(&s)), ["rename /opt/app -> /opt/app2"]);
    }

    #[test]
    fn orphan_names() {
        assert!(is_orphan_name("o840243-38920-0"));
        assert!(is_orphan_name("usr/o1-2-3"));
        assert!(!is_orphan_name("etc/os-release"));
        assert!(!is_orphan_name("o12-3"));
        assert!(!is_orphan_name("oa-b-c"));
    }

    #[test]
    fn releases_say_what_the_old_snapshot_keeps_alone() {
        let s = StreamWriter::new(2)
            // Written in place: two overlapping ranges, then an append past the old end.
            .update_extent("var/db/data", 0, 100)
            .update_extent("var/db/data", 50, 100)
            .update_extent("var/db/data", 4000, 96)
            // Replaced by a new file renamed over it.
            .path(cmd::MKFILE, "o301-9-0")
            .update_extent("o301-9-0", 0, 10)
            .path(cmd::UNLINK, "etc/hosts")
            .rename("o301-9-0", "etc/hosts")
            .path(cmd::UNLINK, "etc/old.conf")
            .path(cmd::CHMOD, "etc/mode-only")
            .finish();
        let cs = changes(&s);
        let release = |p: &str| cs.iter().find(|c| c.path == p).unwrap().release.clone();
        let Release::Part { ranges, .. } = release("/var/db/data") else {
            panic!("in place")
        };
        assert_eq!(ranges, [(0, 150), (4000, 4096)]);
        // Old size 4000: the append releases nothing; the overwrite is one block.
        assert_eq!(release("/var/db/data").bytes(4000, 8192), 4096);
        assert_eq!(
            release("/etc/hosts"),
            Release::Whole {
                from: "/etc/hosts".into()
            }
        );
        assert_eq!(release("/etc/hosts").bytes(30, 4096), 4096);
        assert_eq!(release("/etc/old.conf").from(), Some("/etc/old.conf"));
        assert_eq!(release("/etc/mode-only"), Release::Nothing);
    }

    #[test]
    fn a_truncation_releases_the_tail_and_a_moved_file_its_old_path() {
        let s = StreamWriter::new(2)
            .cmd(
                cmd::TRUNCATE,
                &[
                    (attr::PATH, b"var/log/big"),
                    (attr::SIZE, &1000u64.to_le_bytes()),
                ],
            )
            .rename("opt/a", "opt/b")
            .update_extent("opt/b/f", 0, 10)
            .finish();
        let cs = changes(&s);
        let big = &cs
            .iter()
            .find(|c| c.path == "/var/log/big")
            .unwrap()
            .release;
        assert_eq!(big.bytes(5000, 8192), 4000);
        let f = &cs.iter().find(|c| c.path == "/opt/b/f").unwrap().release;
        assert_eq!(f.from(), Some("/opt/a/f"));
        assert_eq!(f.bytes(100, 4096), 4096);
    }

    #[test]
    fn ranges_merge() {
        let mut r = Vec::new();
        add_range(&mut r, 10, 20);
        add_range(&mut r, 30, 40);
        add_range(&mut r, 20, 30);
        assert_eq!(r, [(10, 40)]);
        for i in 0..100u64 {
            add_range(&mut r, 1000 + i * 10, 1000 + i * 10 + 1);
        }
        assert_eq!((r[0].0, r[r.len() - 1].1), (10, 1991));
    }

    #[test]
    fn a_file_written_all_over_isnt_counted_whole() {
        // journald: 16 bytes in each of 1,000 places, 40 KiB apart, in a 40 MiB file: one
        // block each.
        let mut writer = StreamWriter::new(2);
        for i in 0..1000u64 {
            writer = writer.update_extent("var/log/journal/x/system.journal", i * 40 * 1024, 16);
        }
        let cs = changes(&writer.finish());
        assert_eq!(cs[0].release.bytes(40 << 20, 40 << 20), 1000 * 4096);
        // 10,000 places: too many to keep; the bytes written count, not the file.
        let mut writer = StreamWriter::new(2);
        for i in 0..10_000u64 {
            writer = writer.update_extent("var/log/journal/x/system.journal", i * 4096, 16);
        }
        let cs = changes(&writer.finish());
        let Release::Part { written, .. } = &cs[0].release else {
            panic!("in place")
        };
        assert_eq!(*written, Some(160_000));
        assert_eq!(cs[0].release.bytes(40 << 20, 40 << 20), 160_000);
    }
}
