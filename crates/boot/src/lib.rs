//! The boot side of a metal probe: a temporary BLS entry, armed for one boot, and a record of
//! the default entry that is checked before and after every step.
//!
//! ```text
//! /boot
//! ├── grub2/grubenv               saved_entry=<default id>   next_entry=osreflog-probe (one boot)
//! ├── loader/entries/
//! │   ├── <machine-id>-<ver>.conf    the default: read, hashed, never written
//! │   └── osreflog-probe.conf        the probe: the default's entry with its own kernel and root
//! └── osreflog/                      kernel + initramfs, only when /boot lacks the probe's own
//! ```
//!
//! Arming is `grub2-editenv … set next_entry=osreflog-probe`: GRUB boots that entry once and
//! clears `next_entry` as it does, so any later boot (a hang and a power cycle included) is
//! the default again. `grub2-reboot` does the same `set`, but it also restores a leftover
//! `prev_saved_entry` into `saved_entry`, which would change the default; a block with
//! `prev_saved_entry` is refused instead.

pub mod bls;
pub mod grubenv;
pub mod kernel;
pub mod probe;

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The probe entry's id: its file name without `.conf`, which is what `next_entry` names.
pub const PROBE_ID: &str = "osreflog-probe";

#[derive(Debug, thiserror::Error)]
pub enum BootError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{0}")]
    Invalid(String),
    /// The boot configuration isn't one osreflog will arm a probe in (the reason says why).
    #[error("{0}")]
    Refused(String),
    /// The default entry changed since it was recorded.
    #[error("the default boot entry changed: {0}")]
    DefaultChanged(String),
    #[error("{0}")]
    Tool(String),
}

impl From<osreflog_undo::beneath::BeneathError> for BootError {
    fn from(e: osreflog_undo::beneath::BeneathError) -> BootError {
        use osreflog_undo::beneath::BeneathError;
        match e {
            BeneathError::Open { path, source } => BootError::Io { path, source },
            BeneathError::Refused(what) => BootError::Refused(what),
        }
    }
}

impl BootError {
    fn io(path: &Path, source: std::io::Error) -> BootError {
        BootError::Io {
            path: path.to_owned(),
            source,
        }
    }
}

/// What the default boot is, taken before a probe is armed and compared after every step and
/// at the next boot. Kept in the bisect session, so it outlives the reboots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultRecord {
    /// `saved_entry` in grubenv: the entry GRUB boots when nothing else is asked for.
    pub saved_entry: String,
    /// BLAKE3 of `loader/entries/<saved_entry>.conf`.
    pub entry_blake3: String,
    /// The default entry's kernel and initramfs (relative to `/boot`), which must still exist.
    pub linux: String,
    pub initrd: Vec<String>,
}

/// What a probe left on `/boot`, for `doctor --boot` and for the cleanup after a probe.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Leftovers {
    /// `loader/entries/osreflog-probe.conf` exists.
    pub entry: bool,
    /// `next_entry` in grubenv, if any is set (osreflog's or anyone's).
    pub next_entry: Option<String>,
    /// Files under `/boot/osreflog/`.
    pub files: Vec<PathBuf>,
}

/// `/boot` (or a stand-in for tests) and the `grub2-editenv` to write its grubenv with.
#[derive(Debug, Clone)]
pub struct Boot {
    pub root: PathBuf,
    pub editenv: PathBuf,
}

impl Boot {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Boot {
        Boot {
            root: root.into(),
            editenv: PathBuf::from("grub2-editenv"),
        }
    }

    #[must_use]
    pub fn entries_dir(&self) -> PathBuf {
        self.root.join("loader/entries")
    }

    #[must_use]
    pub fn grubenv_path(&self) -> PathBuf {
        self.root.join("grub2/grubenv")
    }

    #[must_use]
    pub fn probe_entry_path(&self) -> PathBuf {
        self.entries_dir().join(format!("{PROBE_ID}.conf"))
    }

    /// The variables in grubenv.
    ///
    /// # Errors
    /// If grubenv can't be read or isn't a block.
    pub fn env(&self) -> Result<BTreeMap<String, String>, BootError> {
        grubenv::read(&self.grubenv_path())
    }

    fn entry_path(&self, id: &str) -> Result<PathBuf, BootError> {
        if id.is_empty() || id.contains('/') || id.starts_with('.') {
            return Err(BootError::Invalid(format!("not an entry id: {id:?}")));
        }
        Ok(self.entries_dir().join(format!("{id}.conf")))
    }

    /// Records the default entry, refusing a configuration where "the default" isn't one
    /// fixed entry: no `saved_entry` (GRUB would boot the first entry in menu order, which a
    /// probe entry could become), the probe itself, a `prev_saved_entry`, or a `/boot` that
    /// isn't its own filesystem (entries naming `/boot/vmlinuz-…`).
    ///
    /// # Errors
    /// [`BootError::Refused`] for the configurations above; otherwise if grubenv or the entry
    /// can't be read.
    pub fn default_record(&self) -> Result<DefaultRecord, BootError> {
        let env = self.env()?;
        if env.contains_key("prev_saved_entry") {
            return Err(BootError::Refused(
                "grubenv has a prev_saved_entry (left by an older grub2-reboot); \
                 run `grub2-editenv - unset prev_saved_entry` after checking saved_entry"
                    .into(),
            ));
        }
        let saved = env
            .get("saved_entry")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                BootError::Refused(
                    "grubenv has no saved_entry, so the default is \"the first entry\"; \
                 pin it with `grub2-set-default <id>`"
                        .into(),
                )
            })?;
        if saved == PROBE_ID {
            return Err(BootError::Refused(
                "saved_entry names osreflog's probe entry; set the default back first".into(),
            ));
        }
        let path = self.entry_path(saved)?;
        let text = std::fs::read(&path).map_err(|e| BootError::io(&path, e))?;
        let entry = bls::Entry::parse(&String::from_utf8_lossy(&text));
        let linux = entry
            .get("linux")
            .ok_or_else(|| BootError::Invalid(format!("{} has no linux line", path.display())))?
            .to_owned();
        if linux.starts_with("/boot/") {
            return Err(BootError::Refused(
                "/boot is part of the root filesystem here; metal probes need a separate /boot"
                    .into(),
            ));
        }
        let initrd = entry
            .get_all("initrd")
            .into_iter()
            .map(str::to_owned)
            .collect();
        Ok(DefaultRecord {
            saved_entry: saved.clone(),
            entry_blake3: blake3::hash(&text).to_hex().to_string(),
            linux,
            initrd,
        })
    }

    /// Checks that the default is still exactly `rec`: the same `saved_entry`, the same entry
    /// file, its kernel and initramfs present, and no `next_entry` other than the probe's.
    ///
    /// # Errors
    /// [`BootError::DefaultChanged`] with what differs.
    pub fn verify(&self, rec: &DefaultRecord) -> Result<(), BootError> {
        let now = self
            .default_record()
            .map_err(|e| BootError::DefaultChanged(e.to_string()))?;
        if now.saved_entry != rec.saved_entry {
            return Err(BootError::DefaultChanged(format!(
                "saved_entry is {} (was {})",
                now.saved_entry, rec.saved_entry
            )));
        }
        if now.entry_blake3 != rec.entry_blake3 {
            return Err(BootError::DefaultChanged(format!(
                "{}.conf has different content",
                rec.saved_entry
            )));
        }
        for f in std::iter::once(&rec.linux).chain(&rec.initrd) {
            if !self.root.join(f.trim_start_matches('/')).is_file() {
                return Err(BootError::DefaultChanged(format!("{f} is missing")));
            }
        }
        if let Some(next) = self.env()?.get("next_entry").filter(|n| !n.is_empty())
            && next != PROBE_ID
        {
            return Err(BootError::DefaultChanged(format!(
                "next_entry is {next}: something else is armed for the next boot"
            )));
        }
        Ok(())
    }

    /// Writes `loader/entries/osreflog-probe.conf`: the default entry with the probe's kernel,
    /// root subvolume and title, `osreflog.probe=<n>` and `panic=10` (a kernel panic reboots,
    /// into the default). Variables in the default's options (`$kernelopts`, older Fedora) are
    /// expanded from grubenv as GRUB would. Written to a temporary name GRUB ignores, synced,
    /// then renamed into place.
    ///
    /// # Errors
    /// If the default changed since `rec`, or the entry can't be read or written.
    pub fn write_probe_entry(
        &self,
        rec: &DefaultRecord,
        n: u32,
        subvol: &str,
        kernel: &kernel::Plan,
    ) -> Result<PathBuf, BootError> {
        self.verify(rec)?;
        let path = self.entry_path(&rec.saved_entry)?;
        let text = std::fs::read_to_string(&path).map_err(|e| BootError::io(&path, e))?;
        let mut entry = bls::Entry::parse(&text);
        let env = self.env()?;
        let options = expand(entry.get("options").unwrap_or_default(), &env);
        let extra = [format!("osreflog.probe={n}"), "panic=10".to_owned()];
        let (linux, initrd) = kernel.bls_paths();
        entry.set(
            "title",
            &format!("osreflog bisect probe {n} ({})", kernel.version()),
        );
        entry.set("version", kernel.version());
        entry.set("linux", &linux);
        entry.set("initrd", &initrd);
        entry.set("options", &bls::probe_options(&options, subvol, &extra));
        let target = self.probe_entry_path();
        write_synced(&target, entry.render().as_bytes())?;
        Ok(target)
    }

    /// Arms the probe entry for the next boot only, then checks the result: `next_entry` is the
    /// probe, and the default is still `rec`. On a failed check it disarms again.
    ///
    /// # Errors
    /// If the default changed, there is no probe entry, or grub2-editenv fails.
    pub fn arm(&self, rec: &DefaultRecord) -> Result<(), BootError> {
        self.verify(rec)?;
        if !self.probe_entry_path().is_file() {
            return Err(BootError::Invalid("no probe entry to arm".into()));
        }
        grubenv::edit(
            &self.editenv,
            &self.grubenv_path(),
            &["set", &format!("next_entry={PROBE_ID}")],
        )?;
        let armed = self.env()?.get("next_entry").map(String::as_str) == Some(PROBE_ID);
        let checked = self.verify(rec);
        if !armed || checked.is_err() {
            let _ = self.disarm();
            checked?;
            return Err(BootError::Tool(
                "next_entry didn't read back as the probe".into(),
            ));
        }
        Ok(())
    }

    /// What osreflog has on `/boot` right now.
    ///
    /// # Errors
    /// If grubenv can't be read.
    pub fn leftovers(&self) -> Result<Leftovers, BootError> {
        let own = self.root.join(kernel::OWN_DIR);
        let mut files: Vec<PathBuf> = std::fs::read_dir(&own)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect();
        files.sort();
        Ok(Leftovers {
            entry: self.probe_entry_path().exists(),
            next_entry: self
                .env()?
                .get("next_entry")
                .filter(|n| !n.is_empty())
                .cloned(),
            files,
        })
    }

    /// Removes everything a probe put on `/boot`: `next_entry` if it names the probe (never
    /// anyone else's), the probe entry, and `/boot/osreflog/`. Returns what was there.
    ///
    /// # Errors
    /// If grubenv can't be read or edited, or a file can't be removed.
    pub fn disarm(&self) -> Result<Leftovers, BootError> {
        let found = self.leftovers()?;
        if found.next_entry.as_deref() == Some(PROBE_ID) {
            grubenv::edit(
                &self.editenv,
                &self.grubenv_path(),
                &["unset", "next_entry"],
            )?;
        }
        if found.entry {
            let p = self.probe_entry_path();
            std::fs::remove_file(&p).map_err(|e| BootError::io(&p, e))?;
            sync_dir(&self.entries_dir())?;
        }
        if !found.files.is_empty() {
            let own = self.root.join(kernel::OWN_DIR);
            std::fs::remove_dir_all(&own).map_err(|e| BootError::io(&own, e))?;
            sync_dir(&self.root)?;
        }
        Ok(found)
    }
}

/// Expands `$name` and `${name}` from grubenv; an unknown variable is empty, as in GRUB.
#[must_use]
pub fn expand(options: &str, env: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let mut rest = options;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let (name, used) = if let Some(braced) = after.strip_prefix('{') {
            match braced.find('}') {
                Some(end) => (&braced[..end], end + 2),
                None => (braced, braced.len() + 1),
            }
        } else {
            let end = after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            (&after[..end], end)
        };
        if name.is_empty() {
            out.push('$');
        } else {
            out.push_str(env.get(name).map_or("", String::as_str));
        }
        rest = &after[used..];
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Writes `path` through a temporary sibling (`.<name>.tmp`, which `blscfg` doesn't read),
/// syncing the file before the rename and the directory after it.
///
/// # Errors
/// If the directory, the file, the rename or a sync fails.
pub fn write_synced(path: &Path, data: &[u8]) -> Result<(), BootError> {
    let dir = path
        .parent()
        .ok_or_else(|| BootError::Invalid(format!("{} has no parent", path.display())))?;
    std::fs::create_dir_all(dir).map_err(|e| BootError::io(dir, e))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tmp"));
    let mut f = std::fs::File::create(&tmp).map_err(|e| BootError::io(&tmp, e))?;
    f.write_all(data).map_err(|e| BootError::io(&tmp, e))?;
    f.sync_all().map_err(|e| BootError::io(&tmp, e))?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(|e| BootError::io(path, e))?;
    sync_dir(dir)
}

fn sync_dir(dir: &Path) -> Result<(), BootError> {
    std::fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| BootError::io(dir, e))
}

#[cfg(test)]
mod tests;
