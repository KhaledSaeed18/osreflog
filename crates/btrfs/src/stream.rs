//! The btrfs send stream (protocol v1 and v2), parsed incrementally from any reader.
//!
//! Layout: a 17-byte header (`"btrfs-stream\0"` + little-endian `u32` version), then
//! commands. Each command is a 10-byte header (`u32` payload length, `u16` command, `u32`
//! CRC-32C over header-with-zero-CRC + payload) and a payload of TLV attributes (`u16` type,
//! `u16` length, value). In v2 the `DATA` attribute has no length: it runs to the end of the
//! command.

use std::io::{self, Read};

use crate::crc32c;

pub const MAGIC: &[u8; 13] = b"btrfs-stream\0";
const CMD_HEADER_LEN: usize = 10;
/// Larger than any command the kernel emits (it caps commands at 64 KiB + header in v1 and
/// at the encoded-write size in v2); protects against a corrupt length field.
const MAX_PAYLOAD: usize = 16 << 20;

/// Command numbers (`enum btrfs_send_cmd`).
pub mod cmd {
    pub const SUBVOL: u16 = 1;
    pub const SNAPSHOT: u16 = 2;
    pub const MKFILE: u16 = 3;
    pub const MKDIR: u16 = 4;
    pub const MKNOD: u16 = 5;
    pub const MKFIFO: u16 = 6;
    pub const MKSOCK: u16 = 7;
    pub const SYMLINK: u16 = 8;
    pub const RENAME: u16 = 9;
    pub const LINK: u16 = 10;
    pub const UNLINK: u16 = 11;
    pub const RMDIR: u16 = 12;
    pub const SET_XATTR: u16 = 13;
    pub const REMOVE_XATTR: u16 = 14;
    pub const WRITE: u16 = 15;
    pub const CLONE: u16 = 16;
    pub const TRUNCATE: u16 = 17;
    pub const CHMOD: u16 = 18;
    pub const CHOWN: u16 = 19;
    pub const UTIMES: u16 = 20;
    pub const END: u16 = 21;
    pub const UPDATE_EXTENT: u16 = 22;
    pub const FALLOCATE: u16 = 23;
    pub const FILEATTR: u16 = 24;
    pub const ENCODED_WRITE: u16 = 25;
    pub const ENABLE_VERITY: u16 = 26;

    /// The name `btrfs receive --dump` prints for a command.
    #[must_use]
    pub fn name(cmd: u16) -> &'static str {
        match cmd {
            SUBVOL => "subvol",
            SNAPSHOT => "snapshot",
            MKFILE => "mkfile",
            MKDIR => "mkdir",
            MKNOD => "mknod",
            MKFIFO => "mkfifo",
            MKSOCK => "mksock",
            SYMLINK => "symlink",
            RENAME => "rename",
            LINK => "link",
            UNLINK => "unlink",
            RMDIR => "rmdir",
            SET_XATTR => "set_xattr",
            REMOVE_XATTR => "remove_xattr",
            WRITE => "write",
            CLONE => "clone",
            TRUNCATE => "truncate",
            CHMOD => "chmod",
            CHOWN => "chown",
            UTIMES => "utimes",
            END => "end",
            UPDATE_EXTENT => "update_extent",
            FALLOCATE => "fallocate",
            FILEATTR => "fileattr",
            ENCODED_WRITE => "encoded_write",
            ENABLE_VERITY => "enable_verity",
            _ => "unknown",
        }
    }
}

/// Attribute types (`enum btrfs_send_attr`).
pub mod attr {
    pub const UUID: u16 = 1;
    pub const CTRANSID: u16 = 2;
    pub const INO: u16 = 3;
    pub const SIZE: u16 = 4;
    pub const MODE: u16 = 5;
    pub const UID: u16 = 6;
    pub const GID: u16 = 7;
    pub const RDEV: u16 = 8;
    pub const CTIME: u16 = 9;
    pub const MTIME: u16 = 10;
    pub const ATIME: u16 = 11;
    pub const OTIME: u16 = 12;
    pub const XATTR_NAME: u16 = 13;
    pub const XATTR_DATA: u16 = 14;
    pub const PATH: u16 = 15;
    pub const PATH_TO: u16 = 16;
    pub const PATH_LINK: u16 = 17;
    pub const FILE_OFFSET: u16 = 18;
    pub const DATA: u16 = 19;
    pub const CLONE_UUID: u16 = 20;
    pub const CLONE_CTRANSID: u16 = 21;
    pub const CLONE_PATH: u16 = 22;
    pub const CLONE_OFFSET: u16 = 23;
    pub const CLONE_LEN: u16 = 24;
    pub const FALLOCATE_MODE: u16 = 25;
    pub const FILEATTR: u16 = 26;
    pub const UNENCODED_FILE_LEN: u16 = 27;
    pub const UNENCODED_LEN: u16 = 28;
    pub const UNENCODED_OFFSET: u16 = 29;
}

#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    #[error("reading the send stream: {0}")]
    Io(#[from] io::Error),
    #[error("not a btrfs send stream (bad magic)")]
    BadMagic,
    #[error("unsupported send stream version {0}")]
    Version(u32),
    #[error(
        "command {cmd} at offset {offset}: CRC mismatch (stream {stored:#010x}, computed {computed:#010x})"
    )]
    Crc {
        cmd: u16,
        offset: u64,
        stored: u32,
        computed: u32,
    },
    #[error("command {cmd} at offset {offset}: {what}")]
    Malformed {
        cmd: u16,
        offset: u64,
        what: &'static str,
    },
    #[error("stream ended inside a command at offset {0}")]
    Truncated(u64),
    #[error("{cmd}: missing attribute {attr}")]
    MissingAttr { cmd: &'static str, attr: u16 },
}

/// One command with its payload; attributes are ranges into the payload.
#[derive(Debug, Clone)]
pub struct Command {
    pub cmd: u16,
    payload: Vec<u8>,
    attrs: Vec<(u16, std::ops::Range<usize>)>,
}

impl Command {
    #[must_use]
    pub fn name(&self) -> &'static str {
        cmd::name(self.cmd)
    }

    /// The raw value of the first attribute of type `ty`.
    #[must_use]
    pub fn get(&self, ty: u16) -> Option<&[u8]> {
        self.attrs
            .iter()
            .find(|(t, _)| *t == ty)
            .map(|(_, r)| &self.payload[r.clone()])
    }

    /// The value of attribute `ty`, or an error naming the command.
    ///
    /// # Errors
    /// If the attribute is absent.
    pub fn need(&self, ty: u16) -> Result<&[u8], StreamError> {
        self.get(ty).ok_or(StreamError::MissingAttr {
            cmd: self.name(),
            attr: ty,
        })
    }

    /// A little-endian `u64` attribute (shorter values are zero-extended).
    #[must_use]
    pub fn u64(&self, ty: u16) -> Option<u64> {
        let raw = self.get(ty)?;
        let mut buf = [0u8; 8];
        let n = raw.len().min(8);
        buf[..n].copy_from_slice(&raw[..n]);
        Some(u64::from_le_bytes(buf))
    }

    /// Attribute types in stream order.
    pub fn attr_types(&self) -> impl Iterator<Item = u16> + '_ {
        self.attrs.iter().map(|(t, _)| *t)
    }
}

/// Reads commands one at a time.
pub struct StreamReader<R> {
    inner: R,
    version: u32,
    offset: u64,
    done: bool,
    verify_crc: bool,
}

impl<R: Read> StreamReader<R> {
    /// Reads and checks the stream header.
    ///
    /// # Errors
    /// On I/O errors, a wrong magic, or a version other than 1 or 2.
    pub fn new(mut inner: R) -> Result<Self, StreamError> {
        let mut header = [0u8; 17];
        inner.read_exact(&mut header)?;
        if &header[..13] != MAGIC {
            return Err(StreamError::BadMagic);
        }
        let version = u32::from_le_bytes([header[13], header[14], header[15], header[16]]);
        if !(1..=2).contains(&version) {
            return Err(StreamError::Version(version));
        }
        Ok(Self {
            inner,
            version,
            offset: 17,
            done: false,
            verify_crc: true,
        })
    }

    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Skips CRC verification (only for streams known to be produced locally and read once).
    pub fn skip_crc(&mut self) {
        self.verify_crc = false;
    }

    /// The next command, or `None` after `END` or at a clean end of stream.
    ///
    /// # Errors
    /// On I/O errors, CRC mismatches, or malformed commands.
    pub fn next_command(&mut self) -> Result<Option<Command>, StreamError> {
        if self.done {
            return Ok(None);
        }
        let start = self.offset;
        let mut header = [0u8; CMD_HEADER_LEN];
        match read_full(&mut self.inner, &mut header)? {
            0 => {
                self.done = true;
                return Ok(None);
            }
            CMD_HEADER_LEN => {}
            _ => return Err(StreamError::Truncated(start)),
        }
        let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let cmd = u16::from_le_bytes([header[4], header[5]]);
        let stored = u32::from_le_bytes([header[6], header[7], header[8], header[9]]);
        if len > MAX_PAYLOAD {
            return Err(StreamError::Malformed {
                cmd,
                offset: start,
                what: "payload too large",
            });
        }
        let mut payload = vec![0u8; len];
        if read_full(&mut self.inner, &mut payload)? != len {
            return Err(StreamError::Truncated(start));
        }
        self.offset += (CMD_HEADER_LEN + len) as u64;

        if self.verify_crc {
            header[6..10].fill(0);
            let computed = crc32c::update(crc32c::update(0, &header), &payload);
            if computed != stored {
                return Err(StreamError::Crc {
                    cmd,
                    offset: start,
                    stored,
                    computed,
                });
            }
        }

        let attrs = parse_attrs(cmd, &payload, self.version, start)?;
        if cmd == cmd::END {
            self.done = true;
        }
        Ok(Some(Command {
            cmd,
            payload,
            attrs,
        }))
    }
}

fn parse_attrs(
    cmd: u16,
    payload: &[u8],
    version: u32,
    offset: u64,
) -> Result<Vec<(u16, std::ops::Range<usize>)>, StreamError> {
    let malformed = |what| StreamError::Malformed { cmd, offset, what };
    let mut attrs = Vec::new();
    let mut pos = 0;
    while pos < payload.len() {
        if payload.len() - pos < 2 {
            return Err(malformed("truncated attribute type"));
        }
        let ty = u16::from_le_bytes([payload[pos], payload[pos + 1]]);
        if version >= 2 && ty == attr::DATA {
            // v2: no length field; the data runs to the end of the command.
            attrs.push((ty, pos + 2..payload.len()));
            break;
        }
        if payload.len() - pos < 4 {
            return Err(malformed("truncated attribute header"));
        }
        let len = u16::from_le_bytes([payload[pos + 2], payload[pos + 3]]) as usize;
        let start = pos + 4;
        let end = start + len;
        if end > payload.len() {
            return Err(malformed("attribute runs past the command"));
        }
        attrs.push((ty, start..end));
        pos = end;
    }
    Ok(attrs)
}

/// Like `read_exact`, but reports how many bytes were read before EOF instead of failing.
fn read_full<R: Read>(r: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Turns a stream path into text: valid UTF-8 as is, other bytes as `\xNN`.
#[must_use]
pub fn path_text(raw: &[u8]) -> String {
    use std::fmt::Write;
    if let Ok(s) = std::str::from_utf8(raw) {
        return s.to_owned();
    }
    let mut out = String::with_capacity(raw.len() + 8);
    for chunk in raw.utf8_chunks() {
        out.push_str(chunk.valid());
        for b in chunk.invalid() {
            let _ = write!(out, "\\x{b:02x}");
        }
    }
    out
}

/// Builds synthetic streams for tests (here and in other crates).
pub mod writer {
    use super::{CMD_HEADER_LEN, MAGIC, attr};
    use crate::crc32c;

    pub struct StreamWriter {
        buf: Vec<u8>,
        version: u32,
    }

    impl StreamWriter {
        #[must_use]
        pub fn new(version: u32) -> Self {
            let mut buf = MAGIC.to_vec();
            buf.extend_from_slice(&version.to_le_bytes());
            Self { buf, version }
        }

        /// Appends a command with `(type, value)` attributes and a correct CRC.
        ///
        /// # Panics
        /// If an attribute or the command is too long for the format.
        #[must_use]
        pub fn cmd(mut self, cmd: u16, attrs: &[(u16, &[u8])]) -> Self {
            let mut payload = Vec::new();
            for (ty, value) in attrs {
                payload.extend_from_slice(&ty.to_le_bytes());
                if !(self.version >= 2 && *ty == attr::DATA) {
                    let len = u16::try_from(value.len()).unwrap_or(u16::MAX);
                    payload.extend_from_slice(&len.to_le_bytes());
                }
                payload.extend_from_slice(value);
            }
            let len = u32::try_from(payload.len()).unwrap_or(u32::MAX);
            let mut header = [0u8; CMD_HEADER_LEN];
            header[..4].copy_from_slice(&len.to_le_bytes());
            header[4..6].copy_from_slice(&cmd.to_le_bytes());
            let crc = crc32c::update(crc32c::update(0, &header), &payload);
            header[6..].copy_from_slice(&crc.to_le_bytes());
            self.buf.extend_from_slice(&header);
            self.buf.extend_from_slice(&payload);
            self
        }

        /// A command whose only attribute is `PATH`.
        #[must_use]
        pub fn path(self, cmd: u16, path: &str) -> Self {
            self.cmd(cmd, &[(attr::PATH, path.as_bytes())])
        }

        #[must_use]
        pub fn rename(self, from: &str, to: &str) -> Self {
            self.cmd(
                super::cmd::RENAME,
                &[
                    (attr::PATH, from.as_bytes()),
                    (attr::PATH_TO, to.as_bytes()),
                ],
            )
        }

        #[must_use]
        pub fn update_extent(self, path: &str, offset: u64, len: u64) -> Self {
            self.cmd(
                super::cmd::UPDATE_EXTENT,
                &[
                    (attr::PATH, path.as_bytes()),
                    (attr::FILE_OFFSET, &offset.to_le_bytes()),
                    (attr::SIZE, &len.to_le_bytes()),
                ],
            )
        }

        #[must_use]
        pub fn finish(self) -> Vec<u8> {
            self.cmd(super::cmd::END, &[]).buf
        }
    }
}

#[cfg(test)]
mod tests {
    use super::writer::StreamWriter;
    use super::*;

    fn all(bytes: &[u8]) -> Result<Vec<Command>, StreamError> {
        let mut r = StreamReader::new(bytes)?;
        let mut out = Vec::new();
        while let Some(c) = r.next_command()? {
            out.push(c);
        }
        Ok(out)
    }

    #[test]
    fn parses_commands_and_attributes() {
        let bytes = StreamWriter::new(2)
            .path(cmd::MKFILE, "o257-7-0")
            .rename("o257-7-0", "etc/hosts")
            .update_extent("etc/hosts", 0, 4096)
            .finish();
        let cmds = all(&bytes).unwrap();
        assert_eq!(cmds.len(), 4);
        assert_eq!(cmds[1].name(), "rename");
        assert_eq!(cmds[1].get(attr::PATH_TO), Some(&b"etc/hosts"[..]));
        assert_eq!(cmds[2].u64(attr::SIZE), Some(4096));
        assert_eq!(cmds[3].cmd, cmd::END);
    }

    #[test]
    fn v2_data_attribute_runs_to_end_of_command() {
        let bytes = StreamWriter::new(2)
            .cmd(
                cmd::WRITE,
                &[
                    (attr::PATH, b"f"),
                    (attr::FILE_OFFSET, &0u64.to_le_bytes()),
                    (attr::DATA, b"hello"),
                ],
            )
            .finish();
        let cmds = all(&bytes).unwrap();
        assert_eq!(cmds[0].get(attr::DATA), Some(&b"hello"[..]));
    }

    #[test]
    fn rejects_bad_magic_and_version() {
        assert!(matches!(
            StreamReader::new(&b"not-a-stream-at-all"[..]),
            Err(StreamError::BadMagic)
        ));
        let mut bytes = StreamWriter::new(2).finish();
        bytes[13] = 9;
        assert!(matches!(
            StreamReader::new(&bytes[..]),
            Err(StreamError::Version(9))
        ));
    }

    #[test]
    fn detects_corruption() {
        let mut bytes = StreamWriter::new(1).path(cmd::MKDIR, "etc").finish();
        let last = bytes.len() - 11; // inside the mkdir payload
        bytes[last] ^= 0xff;
        assert!(matches!(all(&bytes), Err(StreamError::Crc { .. })));
    }

    #[test]
    fn detects_truncation() {
        let bytes = StreamWriter::new(1).path(cmd::MKDIR, "etc").finish();
        assert!(matches!(
            all(&bytes[..bytes.len() - 3]),
            Err(StreamError::Truncated(_))
        ));
    }

    #[test]
    fn non_utf8_paths_are_escaped() {
        assert_eq!(path_text(b"etc/caf\xe9"), "etc/caf\\xe9");
        assert_eq!(path_text("etc/café".as_bytes()), "etc/café");
    }
}
