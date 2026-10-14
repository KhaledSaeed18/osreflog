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
            cmd::TRUNCATE | cmd::FALLOCATE => self.content(c, 0)?,
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
                },
            );
        }
        // Just inserted or already present.
        self.nodes.get_mut(path).unwrap_or_else(|| unreachable!())
    }

    fn content(&mut self, c: &Command, len: u64) -> Result<(), StreamError> {
        let path = path_text(c.need(attr::PATH)?);
        let node = self.touch(&path);
        node.flags |= flags::CONTENT;
        node.bytes = node.bytes.saturating_add(len);
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
            if node.flags != flags::TIMES || node.origin.is_none() {
                note_parents(path);
            }
        }
        for path in self.deleted.keys() {
            note_parents(path);
        }

        for (path, node) in self.nodes {
            let op = match &node.origin {
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
            out.push(Change {
                path: format!("/{path}"),
                op,
                kind: node.kind,
                flags: node.flags,
                bytes: node.bytes,
            });
        }
        for (path, kind) in self.deleted {
            out.push(Change {
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
    fn orphan_names() {
        assert!(is_orphan_name("o840243-38920-0"));
        assert!(is_orphan_name("usr/o1-2-3"));
        assert!(!is_orphan_name("etc/os-release"));
        assert!(!is_orphan_name("o12-3"));
        assert!(!is_orphan_name("oa-b-c"));
    }
}
