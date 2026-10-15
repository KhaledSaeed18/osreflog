//! The osreflog ledger: checkpoints, the intervals between them (changes, packages, dnf
//! transactions), and per-checkpoint package sets, in one SQLite database owned by the
//! daemon. Every record that matters is appended to a hash chain.

pub mod refs;
pub mod retention;

use std::path::Path;

use osreflog_model::{
    Actor, Cause, CauseRef, Change, Checkpoint, Class, ClassCounts, Gap, IntervalSummary,
    LiveChange, PackageChange, Transaction, VerifyReport,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

pub const SCHEMA_VERSION: i64 = 4;

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("ledger database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("ledger record encoding: {0}")]
    Json(#[from] serde_json::Error),
    #[error("ledger schema version {found} is newer than this osreflog ({supported})")]
    TooNew { found: i64, supported: i64 },
    #[error("no checkpoint {0}")]
    NoCheckpoint(i64),
}

pub type Result<T> = std::result::Result<T, LedgerError>;

const SCHEMA: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE checkpoints (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    created_ms INTEGER NOT NULL,
    reason TEXT NOT NULL,
    generation INTEGER NOT NULL,
    boot_id TEXT NOT NULL,
    note TEXT,
    pm_pid INTEGER,
    pm_comm TEXT,
    pinned INTEGER NOT NULL DEFAULT 0,
    pruned_ms INTEGER
);
CREATE TABLE intervals (
    id INTEGER PRIMARY KEY,
    from_id INTEGER NOT NULL REFERENCES checkpoints(id),
    to_id INTEGER NOT NULL REFERENCES checkpoints(id),
    computed_ms INTEGER NOT NULL,
    duration_ms INTEGER NOT NULL,
    stream_bytes INTEGER NOT NULL,
    commands INTEGER NOT NULL,
    changed_bytes INTEGER NOT NULL,
    UNIQUE (from_id, to_id)
);
CREATE TABLE changes (
    interval_id INTEGER NOT NULL REFERENCES intervals(id),
    path TEXT NOT NULL,
    op TEXT NOT NULL,
    from_path TEXT,
    kind TEXT NOT NULL,
    class TEXT NOT NULL,
    what TEXT NOT NULL,
    bytes INTEGER NOT NULL
);
CREATE INDEX changes_by_interval ON changes (interval_id, class);
CREATE INDEX changes_by_path ON changes (path);
CREATE TABLE packages (
    interval_id INTEGER NOT NULL REFERENCES intervals(id),
    name TEXT NOT NULL,
    arch TEXT NOT NULL,
    action TEXT NOT NULL,
    from_evr TEXT,
    to_evr TEXT
);
CREATE INDEX packages_by_interval ON packages (interval_id);
CREATE TABLE transactions (
    interval_id INTEGER NOT NULL REFERENCES intervals(id),
    id INTEGER NOT NULL,
    begin_s INTEGER NOT NULL,
    end_s INTEGER,
    user_id INTEGER NOT NULL,
    description TEXT NOT NULL,
    state TEXT NOT NULL,
    PRIMARY KEY (interval_id, id)
);
CREATE TABLE package_sets (
    checkpoint_id INTEGER PRIMARY KEY REFERENCES checkpoints(id),
    same_as INTEGER REFERENCES checkpoints(id),
    data BLOB
);
CREATE TABLE chain (
    seq INTEGER PRIMARY KEY,
    kind TEXT NOT NULL,
    ref_id INTEGER NOT NULL,
    record TEXT NOT NULL,
    hash BLOB NOT NULL
);
";

/// v1 → v2 (P3): what the live witness saw.
const SCHEMA_V2: &str = "
CREATE TABLE causes (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL,
    label TEXT NOT NULL,
    actor TEXT NOT NULL,
    started_ms INTEGER NOT NULL,
    ended_ms INTEGER NOT NULL,
    package_paths INTEGER NOT NULL
);
CREATE INDEX causes_by_time ON causes (started_ms);
CREATE TABLE live_changes (
    cause_id INTEGER NOT NULL REFERENCES causes(id),
    ts_ms INTEGER NOT NULL,
    path TEXT NOT NULL,
    op TEXT NOT NULL,
    from_path TEXT,
    kind TEXT NOT NULL,
    blob TEXT,
    size INTEGER
);
CREATE INDEX live_by_path ON live_changes (path, ts_ms);
CREATE INDEX live_by_time ON live_changes (ts_ms);
CREATE TABLE gaps (
    id INTEGER PRIMARY KEY,
    start_ms INTEGER NOT NULL,
    end_ms INTEGER NOT NULL,
    reason TEXT NOT NULL
);
CREATE TABLE attribution (
    interval_id INTEGER NOT NULL REFERENCES intervals(id),
    path TEXT NOT NULL,
    cause_id INTEGER REFERENCES causes(id),
    witness TEXT NOT NULL
);
CREATE INDEX attribution_by_interval ON attribution (interval_id, path);
";

/// v2 → v3: which process made each live change.
const SCHEMA_V3: &str = "ALTER TABLE live_changes ADD COLUMN actor TEXT;";

/// v3 → v4: symlink targets on live changes, and per-change semantic summaries (derived from
/// blobs, so not chained: they can always be recomputed).
const SCHEMA_V4: &str = "
ALTER TABLE live_changes ADD COLUMN target TEXT;
CREATE TABLE summaries (
    cause_id INTEGER NOT NULL REFERENCES causes(id),
    path TEXT NOT NULL,
    lines TEXT NOT NULL,
    PRIMARY KEY (cause_id, path)
);
";

/// An interval as stored or about to be stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interval {
    pub from_id: i64,
    pub to_id: i64,
    pub computed_ms: i64,
    pub duration_ms: i64,
    pub stream_bytes: u64,
    pub commands: u64,
    pub changed_bytes: u64,
    pub changes: Vec<Change>,
    pub packages: Vec<PackageChange>,
    pub transactions: Vec<Transaction>,
}

/// The immutable part of a checkpoint, as hashed into the chain.
#[derive(Serialize)]
struct CheckpointRecord<'a> {
    id: i64,
    name: &'a str,
    created_ms: i64,
    reason: &'a str,
    generation: u64,
    boot_id: &'a str,
    note: Option<&'a str>,
    pm_pid: Option<u32>,
    pm_comm: Option<&'a str>,
}

impl<'a> From<&'a Checkpoint> for CheckpointRecord<'a> {
    fn from(c: &'a Checkpoint) -> Self {
        Self {
            id: c.id,
            name: &c.name,
            created_ms: c.created_ms,
            reason: &c.reason,
            generation: c.generation,
            boot_id: &c.boot_id,
            note: c.note.as_deref(),
            pm_pid: c.pm_pid,
            pm_comm: c.pm_comm.as_deref(),
        }
    }
}

#[derive(Serialize)]
struct IntervalRecord {
    from_id: i64,
    to_id: i64,
    computed_ms: i64,
    duration_ms: i64,
    stream_bytes: u64,
    commands: u64,
    changed_bytes: u64,
    changes: String,
    packages: String,
    transactions: String,
}

fn digest<T: Serialize>(value: &T) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(value)?)
        .to_hex()
        .to_string())
}

impl IntervalRecord {
    /// Attribution (cause, witness) is not part of the interval's record: it's chained on its
    /// own (`attribution`), so intervals stored before P3 still verify.
    fn of(iv: &Interval) -> Result<Self> {
        let changes: Vec<Change> = iv
            .changes
            .iter()
            .map(|c| Change {
                cause: None,
                witness: None,
                summary: Vec::new(),
                ..c.clone()
            })
            .collect();
        Ok(Self {
            from_id: iv.from_id,
            to_id: iv.to_id,
            computed_ms: iv.computed_ms,
            duration_ms: iv.duration_ms,
            stream_bytes: iv.stream_bytes,
            commands: iv.commands,
            changed_bytes: iv.changed_bytes,
            changes: digest(&changes)?,
            packages: digest(&iv.packages)?,
            transactions: digest(&iv.transactions)?,
        })
    }
}

#[derive(Serialize)]
struct CauseRecord<'a> {
    id: i64,
    kind: &'a str,
    label: &'a str,
    actor: &'a Actor,
    started_ms: i64,
    ended_ms: i64,
    package_paths: u64,
    changes: String,
}

impl<'a> CauseRecord<'a> {
    fn of(c: &'a Cause, changes: &[LiveChange]) -> Result<Self> {
        Ok(Self {
            id: c.id,
            kind: &c.kind,
            label: &c.label,
            actor: &c.actor,
            started_ms: c.started_ms,
            ended_ms: c.ended_ms,
            package_paths: c.package_paths,
            changes: digest(&changes)?,
        })
    }
}

#[derive(Serialize)]
struct AttributionRecord {
    interval_id: i64,
    rows: String,
}

/// One change's attribution within an interval: its path, its cause (if any), its witness.
pub type Attribution = (String, Option<i64>, String);

#[derive(Serialize, serde::Deserialize, PartialEq, Eq)]
struct PruneRecord {
    id: i64,
    ms: i64,
}

#[derive(Serialize, serde::Deserialize, PartialEq, Eq)]
struct PinRecord {
    id: i64,
    pinned: bool,
}

fn chain_hash(prev: &[u8; 32], kind: &str, record: &str) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(prev);
    h.update(kind.as_bytes());
    h.update(&[0]);
    h.update(record.as_bytes());
    *h.finalize().as_bytes()
}

fn i64_of(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn u64_of(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

pub struct Ledger {
    conn: Connection,
}

/// Where a user's own ledger lives (user scope, written by `osreflog-user`):
/// `$XDG_STATE_HOME/osreflog/user.db`, else `~/.local/state/osreflog/user.db`.
#[must_use]
pub fn user_ledger_path() -> Option<std::path::PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/state"))
        })?;
    Some(state.join("osreflog").join("user.db"))
}

impl Ledger {
    /// Opens (creating and migrating if needed) the ledger at `path`.
    ///
    /// # Errors
    /// On SQLite errors or a schema newer than this build.
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    /// An in-memory ledger (tests).
    ///
    /// # Errors
    /// On SQLite errors.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(LedgerError::TooNew {
                found: version,
                supported: SCHEMA_VERSION,
            });
        }
        if version == 0 {
            conn.execute_batch(SCHEMA)?;
        }
        if version < 2 {
            conn.execute_batch(SCHEMA_V2)?;
        }
        if version < 3 {
            conn.execute_batch(SCHEMA_V3)?;
        }
        if version < 4 {
            conn.execute_batch(SCHEMA_V4)?;
        }
        if version < SCHEMA_VERSION {
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(Self { conn })
    }

    /// # Errors
    /// On SQLite errors.
    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    /// # Errors
    /// On SQLite errors.
    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    fn append_chain(
        tx: &rusqlite::Transaction<'_>,
        kind: &str,
        ref_id: i64,
        record: &str,
    ) -> Result<()> {
        let prev: Option<Vec<u8>> = tx
            .query_row(
                "SELECT hash FROM chain ORDER BY seq DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let mut prev_hash = [0u8; 32];
        if let Some(p) = prev
            && p.len() == 32
        {
            prev_hash.copy_from_slice(&p);
        }
        let hash = chain_hash(&prev_hash, kind, record);
        tx.execute(
            "INSERT INTO chain (kind, ref_id, record, hash) VALUES (?1, ?2, ?3, ?4)",
            params![kind, ref_id, record, hash.as_slice()],
        )?;
        Ok(())
    }

    /// The id the next checkpoint will get (the snapshot is named after it before the row
    /// exists; the daemon is the only writer).
    ///
    /// # Errors
    /// On SQLite errors.
    pub fn next_checkpoint_id(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(MAX(id), 0) + 1 FROM checkpoints",
            [],
            |r| r.get(0),
        )?)
    }

    /// # Errors
    /// On SQLite errors, including a duplicate id or name.
    pub fn add_checkpoint(&mut self, c: &Checkpoint) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO checkpoints (id, name, created_ms, reason, generation, boot_id, note, pm_pid, pm_comm, pinned, pruned_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                c.id,
                c.name,
                c.created_ms,
                c.reason,
                i64_of(c.generation),
                c.boot_id,
                c.note,
                c.pm_pid,
                c.pm_comm,
                c.pinned,
                c.pruned_ms
            ],
        )?;
        let record = serde_json::to_string(&CheckpointRecord::from(c))?;
        Self::append_chain(&tx, "checkpoint", c.id, &record)?;
        tx.commit()?;
        Ok(())
    }

    fn row_to_checkpoint(r: &rusqlite::Row<'_>) -> rusqlite::Result<Checkpoint> {
        Ok(Checkpoint {
            id: r.get(0)?,
            name: r.get(1)?,
            created_ms: r.get(2)?,
            reason: r.get(3)?,
            generation: u64_of(r.get(4)?),
            boot_id: r.get(5)?,
            note: r.get(6)?,
            pm_pid: r.get(7)?,
            pm_comm: r.get(8)?,
            pinned: r.get(9)?,
            pruned_ms: r.get(10)?,
        })
    }

    const CHECKPOINT_COLS: &'static str = "id, name, created_ms, reason, generation, boot_id, note, pm_pid, pm_comm, pinned, pruned_ms";

    /// All checkpoints, oldest first.
    ///
    /// # Errors
    /// On SQLite errors.
    pub fn checkpoints(&self) -> Result<Vec<Checkpoint>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM checkpoints ORDER BY id",
            Self::CHECKPOINT_COLS
        ))?;
        let rows = stmt.query_map([], Self::row_to_checkpoint)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// # Errors
    /// On SQLite errors.
    pub fn checkpoint(&self, id: i64) -> Result<Option<Checkpoint>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {} FROM checkpoints WHERE id = ?1",
                    Self::CHECKPOINT_COLS
                ),
                [id],
                Self::row_to_checkpoint,
            )
            .optional()?)
    }

    /// # Errors
    /// On SQLite errors.
    pub fn latest(&self) -> Result<Option<Checkpoint>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {} FROM checkpoints ORDER BY id DESC LIMIT 1",
                    Self::CHECKPOINT_COLS
                ),
                [],
                Self::row_to_checkpoint,
            )
            .optional()?)
    }

    /// The checkpoint just before `id`.
    ///
    /// # Errors
    /// On SQLite errors.
    pub fn previous(&self, id: i64) -> Result<Option<Checkpoint>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {} FROM checkpoints WHERE id < ?1 ORDER BY id DESC LIMIT 1",
                    Self::CHECKPOINT_COLS
                ),
                [id],
                Self::row_to_checkpoint,
            )
            .optional()?)
    }

    /// Records that a checkpoint's snapshot was deleted at `ms`.
    ///
    /// # Errors
    /// On SQLite errors, or if there is no such checkpoint.
    pub fn mark_pruned(&mut self, id: i64, ms: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        if tx.execute(
            "UPDATE checkpoints SET pruned_ms = ?2 WHERE id = ?1",
            params![id, ms],
        )? == 0
        {
            return Err(LedgerError::NoCheckpoint(id));
        }
        Self::append_chain(
            &tx,
            "prune",
            id,
            &serde_json::to_string(&PruneRecord { id, ms })?,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// # Errors
    /// On SQLite errors, or if there is no such checkpoint.
    pub fn set_pinned(&mut self, id: i64, pinned: bool) -> Result<()> {
        let tx = self.conn.transaction()?;
        if tx.execute(
            "UPDATE checkpoints SET pinned = ?2 WHERE id = ?1",
            params![id, pinned],
        )? == 0
        {
            return Err(LedgerError::NoCheckpoint(id));
        }
        Self::append_chain(
            &tx,
            "pin",
            id,
            &serde_json::to_string(&PinRecord { id, pinned })?,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Stores a computed interval with its changes, packages and transactions.
    ///
    /// # Errors
    /// On SQLite errors, including an interval already stored for this pair.
    pub fn store_interval(&mut self, iv: &Interval) -> Result<i64> {
        let record = serde_json::to_string(&IntervalRecord::of(iv)?)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO intervals (from_id, to_id, computed_ms, duration_ms, stream_bytes, commands, changed_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                iv.from_id,
                iv.to_id,
                iv.computed_ms,
                iv.duration_ms,
                i64_of(iv.stream_bytes),
                i64_of(iv.commands),
                i64_of(iv.changed_bytes)
            ],
        )?;
        let id = tx.last_insert_rowid();
        {
            let mut ins = tx.prepare(
                "INSERT INTO changes (interval_id, path, op, from_path, kind, class, what, bytes) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for c in &iv.changes {
                ins.execute(params![
                    id,
                    c.path,
                    c.op,
                    c.from,
                    c.kind,
                    c.class.as_str(),
                    c.what.join(","),
                    i64_of(c.bytes)
                ])?;
            }
            let mut ins = tx.prepare(
                "INSERT INTO packages (interval_id, name, arch, action, from_evr, to_evr) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for p in &iv.packages {
                ins.execute(params![id, p.name, p.arch, p.action, p.from_evr, p.to_evr])?;
            }
            let mut ins = tx.prepare(
                "INSERT OR IGNORE INTO transactions (interval_id, id, begin_s, end_s, user_id, description, state) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for t in &iv.transactions {
                ins.execute(params![
                    id,
                    t.id,
                    t.begin,
                    t.end,
                    t.user_id,
                    t.description,
                    t.state
                ])?;
            }
        }
        Self::append_chain(&tx, "interval", id, &record)?;
        tx.commit()?;
        Ok(id)
    }

    fn interval_row(&self, id: i64) -> Result<Interval> {
        let mut iv = self.conn.query_row(
            "SELECT from_id, to_id, computed_ms, duration_ms, stream_bytes, commands, changed_bytes FROM intervals WHERE id = ?1",
            [id],
            |r| {
                Ok(Interval {
                    from_id: r.get(0)?,
                    to_id: r.get(1)?,
                    computed_ms: r.get(2)?,
                    duration_ms: r.get(3)?,
                    stream_bytes: u64_of(r.get(4)?),
                    commands: u64_of(r.get(5)?),
                    changed_bytes: u64_of(r.get(6)?),
                    changes: Vec::new(),
                    packages: Vec::new(),
                    transactions: Vec::new(),
                })
            },
        )?;
        let mut stmt = self.conn.prepare(
            "SELECT path, op, from_path, kind, class, what, bytes FROM changes WHERE interval_id = ?1 ORDER BY rowid",
        )?;
        iv.changes = stmt
            .query_map([id], |r| {
                let class: String = r.get(4)?;
                let what: String = r.get(5)?;
                Ok(Change {
                    path: r.get(0)?,
                    op: r.get(1)?,
                    from: r.get(2)?,
                    kind: r.get(3)?,
                    class: Class::parse(&class).unwrap_or(Class::Other),
                    what: what
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect(),
                    bytes: u64_of(r.get(6)?),
                    cause: None,
                    witness: None,
                    summary: Vec::new(),
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        let attribution = self.attribution_of(id)?;
        if !attribution.is_empty() {
            for c in &mut iv.changes {
                if let Some((cause, witness)) = attribution.get(&c.path) {
                    c.cause.clone_from(cause);
                    c.witness = Some(witness.clone());
                }
            }
        }
        let mut stmt = self.conn.prepare(
            "SELECT name, arch, action, from_evr, to_evr FROM packages WHERE interval_id = ?1 ORDER BY rowid",
        )?;
        iv.packages = stmt
            .query_map([id], |r| {
                Ok(PackageChange {
                    name: r.get(0)?,
                    arch: r.get(1)?,
                    action: r.get(2)?,
                    from_evr: r.get(3)?,
                    to_evr: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        iv.transactions = self.transactions_of(id)?;
        Ok(iv)
    }

    fn transactions_of(&self, id: i64) -> Result<Vec<Transaction>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, begin_s, end_s, user_id, description, state FROM transactions WHERE interval_id = ?1 ORDER BY id",
        )?;
        Ok(stmt
            .query_map([id], |r| {
                Ok(Transaction {
                    id: r.get(0)?,
                    begin: r.get(1)?,
                    end: r.get(2)?,
                    user_id: r.get(3)?,
                    description: r.get(4)?,
                    state: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?)
    }

    /// The stored interval between two checkpoints, if computed.
    ///
    /// # Errors
    /// On SQLite errors.
    pub fn interval(&self, from_id: i64, to_id: i64) -> Result<Option<Interval>> {
        let id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM intervals WHERE from_id = ?1 AND to_id = ?2",
                [from_id, to_id],
                |r| r.get(0),
            )
            .optional()?;
        id.map(|id| self.interval_row(id)).transpose()
    }

    /// Counts and packages for the interval ending at `to_id` (from its predecessor).
    ///
    /// # Errors
    /// On SQLite errors.
    pub fn summary_ending_at(&self, to_id: i64) -> Result<Option<IntervalSummary>> {
        let row: Option<(i64, i64)> = self
            .conn
            .query_row(
                "SELECT id, from_id FROM intervals WHERE to_id = ?1 ORDER BY from_id DESC LIMIT 1",
                [to_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((id, from_id)) = row else {
            return Ok(None);
        };
        let mut counts = ClassCounts::default();
        let mut stmt = self
            .conn
            .prepare("SELECT class, COUNT(*) FROM changes WHERE interval_id = ?1 GROUP BY class")?;
        let rows = stmt.query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (class, n) = row?;
            let n = u64_of(n);
            match Class::parse(&class).unwrap_or(Class::Other) {
                Class::Config => counts.config += n,
                Class::State => counts.state += n,
                Class::Package => counts.package += n,
                Class::Noise => counts.noise += n,
                Class::Other => counts.other += n,
            }
        }
        let packages: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM packages WHERE interval_id = ?1",
            [id],
            |r| r.get(0),
        )?;
        Ok(Some(IntervalSummary {
            from_id,
            counts,
            packages: u64_of(packages),
            transactions: self.transactions_of(id)?,
        }))
    }

    /// Consecutive checkpoint pairs (by id) without a stored interval, whose snapshots both
    /// still exist: the diffs still to compute.
    ///
    /// # Errors
    /// On SQLite errors.
    pub fn pending_intervals(&self) -> Result<Vec<(i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.id, c.id FROM checkpoints c
             JOIN checkpoints p ON p.id = (SELECT MAX(id) FROM checkpoints WHERE id < c.id)
             WHERE c.pruned_ms IS NULL AND p.pruned_ms IS NULL
               AND NOT EXISTS (SELECT 1 FROM intervals i WHERE i.from_id = p.id AND i.to_id = c.id)
             ORDER BY c.id",
        )?;
        Ok(stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?)
    }

    /// Stores a checkpoint's package set (opaque bytes), or records that it equals the set of
    /// checkpoint `same_as`.
    ///
    /// # Errors
    /// On SQLite errors.
    pub fn store_package_set(
        &self,
        checkpoint_id: i64,
        data: Option<&[u8]>,
        same_as: Option<i64>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO package_sets (checkpoint_id, same_as, data) VALUES (?1, ?2, ?3)",
            params![checkpoint_id, same_as, data],
        )?;
        Ok(())
    }

    /// A checkpoint's package set, following `same_as` links; `None` if never stored.
    ///
    /// # Errors
    /// On SQLite errors.
    pub fn package_set(&self, checkpoint_id: i64) -> Result<Option<(i64, Vec<u8>)>> {
        let mut id = checkpoint_id;
        for _ in 0..10_000 {
            let row: Option<(Option<i64>, Option<Vec<u8>>)> = self
                .conn
                .query_row(
                    "SELECT same_as, data FROM package_sets WHERE checkpoint_id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            match row {
                Some((_, Some(data))) => return Ok(Some((id, data))),
                Some((Some(next), None)) => id = next,
                _ => return Ok(None),
            }
        }
        Ok(None)
    }

    /// # Errors
    /// On SQLite errors.
    pub fn chain_len(&self) -> Result<u64> {
        Ok(u64_of(self.conn.query_row(
            "SELECT COUNT(*) FROM chain",
            [],
            |r| r.get(0),
        )?))
    }

    /// Re-hashes the whole chain and checks every checkpoint and interval record against the
    /// tables, and the final pin/prune state of every checkpoint against its last event.
    ///
    /// # Errors
    /// On SQLite errors (a mismatch is a report, not an error).
    pub fn verify(&self) -> Result<VerifyReport> {
        let mut stmt = self
            .conn
            .prepare("SELECT seq, kind, ref_id, record, hash FROM chain ORDER BY seq")?;
        let rows: Vec<(i64, String, i64, String, Vec<u8>)> = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<std::result::Result<_, _>>()?;
        let mut prev = [0u8; 32];
        let mut last_prune = std::collections::HashMap::new();
        let mut last_pin = std::collections::HashMap::new();
        let bad = |seq: i64, detail: String| VerifyReport {
            entries: u64_of(i64::try_from(rows.len()).unwrap_or(0)),
            ok: false,
            first_bad: Some(u64_of(seq)),
            detail: Some(detail),
        };
        for (seq, kind, ref_id, record, hash) in &rows {
            if chain_hash(&prev, kind, record).as_slice() != hash.as_slice() {
                return Ok(bad(
                    *seq,
                    format!("{kind} {ref_id}: hash does not match the chain"),
                ));
            }
            prev.copy_from_slice(hash);
            let current = match kind.as_str() {
                "checkpoint" => match self.checkpoint(*ref_id)? {
                    Some(c) => Some(serde_json::to_string(&CheckpointRecord::from(&c))?),
                    None => None,
                },
                "interval" => match self.interval_row(*ref_id) {
                    Ok(iv) => Some(serde_json::to_string(&IntervalRecord::of(&iv)?)?),
                    Err(LedgerError::Sqlite(rusqlite::Error::QueryReturnedNoRows)) => None,
                    Err(e) => return Err(e),
                },
                "cause" => match self.cause(*ref_id)? {
                    Some(c) => Some(serde_json::to_string(&CauseRecord::of(
                        &c,
                        &self.cause_changes(*ref_id)?,
                    )?)?),
                    None => None,
                },
                "gap" => self
                    .gap(*ref_id)?
                    .map(|g| serde_json::to_string(&g))
                    .transpose()?,
                "attribution" => Some(serde_json::to_string(&AttributionRecord {
                    interval_id: *ref_id,
                    rows: digest(&self.attribution_rows(*ref_id)?)?,
                })?),
                "prune" => {
                    let r: PruneRecord = serde_json::from_str(record)?;
                    last_prune.insert(r.id, r.ms);
                    continue;
                }
                "pin" => {
                    let r: PinRecord = serde_json::from_str(record)?;
                    last_pin.insert(r.id, r.pinned);
                    continue;
                }
                _ => return Ok(bad(*seq, format!("unknown chain entry kind {kind}"))),
            };
            if current.as_deref() != Some(record.as_str()) {
                return Ok(bad(
                    *seq,
                    format!("{kind} {ref_id}: the table no longer matches the chained record"),
                ));
            }
        }
        for c in self.checkpoints()? {
            if c.pruned_ms != last_prune.get(&c.id).copied() {
                return Ok(bad(
                    0,
                    format!("checkpoint {}: pruned state not backed by the chain", c.id),
                ));
            }
            if c.pinned != last_pin.get(&c.id).copied().unwrap_or(false) {
                return Ok(bad(
                    0,
                    format!("checkpoint {}: pinned state not backed by the chain", c.id),
                ));
            }
        }
        Ok(VerifyReport {
            entries: u64_of(i64::try_from(rows.len()).unwrap_or(0)),
            ok: true,
            first_bad: None,
            detail: None,
        })
    }

    /// The id of the stored interval between two checkpoints.
    ///
    /// # Errors
    /// On SQLite errors.
    pub fn interval_id(&self, from_id: i64, to_id: i64) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM intervals WHERE from_id = ?1 AND to_id = ?2",
                [from_id, to_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Stores a closed cause and its changes; returns its id.
    ///
    /// # Errors
    /// On SQLite or encoding errors.
    pub fn store_cause(&mut self, cause: &Cause, changes: &[LiveChange]) -> Result<i64> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO causes (kind, label, actor, started_ms, ended_ms, package_paths) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                cause.kind,
                cause.label,
                serde_json::to_string(&cause.actor)?,
                cause.started_ms,
                cause.ended_ms,
                i64_of(cause.package_paths)
            ],
        )?;
        let id = tx.last_insert_rowid();
        {
            let mut ins = tx.prepare(
                "INSERT INTO live_changes (cause_id, ts_ms, path, op, from_path, kind, blob, size, actor, target) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;
            for c in changes {
                ins.execute(params![
                    id,
                    c.ts_ms,
                    c.path,
                    c.op,
                    c.from,
                    c.kind,
                    c.blob,
                    c.size.map(i64_of),
                    c.actor,
                    c.target
                ])?;
            }
        }
        let stored = Cause {
            id,
            changes: changes.len() as u64,
            ..cause.clone()
        };
        Self::append_chain(
            &tx,
            "cause",
            id,
            &serde_json::to_string(&CauseRecord::of(&stored, changes)?)?,
        )?;
        tx.commit()?;
        Ok(id)
    }

    fn row_to_cause(r: &rusqlite::Row<'_>) -> rusqlite::Result<Cause> {
        let actor: String = r.get(3)?;
        Ok(Cause {
            id: r.get(0)?,
            kind: r.get(1)?,
            label: r.get(2)?,
            actor: serde_json::from_str(&actor).unwrap_or_default(),
            started_ms: r.get(4)?,
            ended_ms: r.get(5)?,
            package_paths: u64_of(r.get(6)?),
            changes: u64_of(r.get(7)?),
        })
    }

    const CAUSE_SELECT: &'static str =
        "SELECT c.id, c.kind, c.label, c.actor, c.started_ms, c.ended_ms, c.package_paths,
        (SELECT COUNT(*) FROM live_changes l WHERE l.cause_id = c.id) FROM causes c";

    /// # Errors
    /// On SQLite errors.
    pub fn cause(&self, id: i64) -> Result<Option<Cause>> {
        Ok(self
            .conn
            .query_row(
                &format!("{} WHERE c.id = ?1", Self::CAUSE_SELECT),
                [id],
                Self::row_to_cause,
            )
            .optional()?)
    }

    /// Causes newest first, at most `limit`, optionally only those that ended at or after
    /// `since_ms`.
    ///
    /// # Errors
    /// On SQLite errors.
    pub fn causes(&self, limit: usize, since_ms: Option<i64>) -> Result<Vec<Cause>> {
        let mut stmt = self.conn.prepare(&format!(
            "{} WHERE c.ended_ms >= ?1 ORDER BY c.started_ms DESC, c.id DESC LIMIT ?2",
            Self::CAUSE_SELECT
        ))?;
        let rows = stmt.query_map(
            params![
                since_ms.unwrap_or(i64::MIN),
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
            Self::row_to_cause,
        )?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    fn row_to_live(r: &rusqlite::Row<'_>, at: usize) -> rusqlite::Result<LiveChange> {
        let size: Option<i64> = r.get(at + 6)?;
        Ok(LiveChange {
            ts_ms: r.get(at)?,
            path: r.get(at + 1)?,
            op: r.get(at + 2)?,
            from: r.get(at + 3)?,
            kind: r.get(at + 4)?,
            blob: r.get(at + 5)?,
            size: size.map(u64_of),
            actor: r.get(at + 7)?,
            target: r.get(at + 8)?,
            summary: Vec::new(),
        })
    }

    /// A cause's changes in the order they happened.
    ///
    /// # Errors
    /// On SQLite errors.
    pub fn cause_changes(&self, id: i64) -> Result<Vec<LiveChange>> {
        let mut stmt = self.conn.prepare(
            "SELECT ts_ms, path, op, from_path, kind, blob, size, actor, target FROM live_changes WHERE cause_id = ?1 ORDER BY rowid",
        )?;
        let rows = stmt.query_map([id], |r| Self::row_to_live(r, 0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Live changes in `[from_ms, to_ms]`, with their cause ids, oldest first.
    ///
    /// # Errors
    /// On SQLite errors.
    pub fn live_changes_between(&self, from_ms: i64, to_ms: i64) -> Result<Vec<(i64, LiveChange)>> {
        let mut stmt = self.conn.prepare(
            "SELECT cause_id, ts_ms, path, op, from_path, kind, blob, size, actor, target FROM live_changes
             WHERE ts_ms BETWEEN ?1 AND ?2 ORDER BY ts_ms, rowid",
        )?;
        let rows = stmt.query_map([from_ms, to_ms], |r| {
            Ok((r.get(0)?, Self::row_to_live(r, 1)?))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Causes active in `[from_ms, to_ms]` of a given kind (e.g. `dnf`).
    ///
    /// # Errors
    /// On SQLite errors.
    pub fn causes_between(&self, kind: &str, from_ms: i64, to_ms: i64) -> Result<Vec<Cause>> {
        let mut stmt = self.conn.prepare(&format!(
            "{} WHERE c.kind = ?1 AND c.ended_ms >= ?2 AND c.started_ms <= ?3 ORDER BY c.started_ms",
            Self::CAUSE_SELECT
        ))?;
        let rows = stmt.query_map(params![kind, from_ms, to_ms], Self::row_to_cause)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// The newest `limit` live changes to `path` (as its new or old name), newest first, with
    /// their causes.
    ///
    /// # Errors
    /// On SQLite errors.
    pub fn history(&self, path: &str, limit: usize) -> Result<Vec<(Cause, LiveChange)>> {
        let mut stmt = self.conn.prepare(
            "SELECT cause_id, ts_ms, path, op, from_path, kind, blob, size, actor, target FROM live_changes
             WHERE path = ?1 OR from_path = ?1 ORDER BY ts_ms DESC, rowid DESC LIMIT ?2",
        )?;
        let rows: Vec<(i64, LiveChange)> = stmt
            .query_map(
                params![path, i64::try_from(limit).unwrap_or(i64::MAX)],
                |r| Ok((r.get(0)?, Self::row_to_live(r, 1)?)),
            )?
            .collect::<std::result::Result<_, _>>()?;
        let mut out = Vec::new();
        for (cause_id, change) in rows {
            if let Some(c) = self.cause(cause_id)? {
                out.push((c, change));
            }
        }
        Ok(out)
    }

    /// The last time before `ts_ms` that `path` went away: deleted or renamed, itself or a
    /// directory above it. Content recorded before that belongs to an earlier file.
    ///
    /// # Errors
    /// On SQLite errors.
    pub fn last_gone(&self, path: &str, ts_ms: i64) -> Result<Option<i64>> {
        Ok(self.conn.query_row(
            "SELECT MAX(ts_ms) FROM live_changes WHERE ts_ms < ?2 AND (
                (op = 'delete' AND (path = ?1 OR substr(?1, 1, length(path) + 1) = path || '/'))
                OR (op = 'rename' AND from_path IS NOT NULL
                    AND (from_path = ?1 OR substr(?1, 1, length(from_path) + 1) = from_path || '/')))",
            params![path, ts_ms],
            |r| r.get(0),
        )?)
    }

    /// The newest content blob recorded for `path` before `ts_ms`, and since it last went
    /// away (see [`Self::last_gone`]).
    ///
    /// # Errors
    /// On SQLite errors.
    pub fn previous_blob(&self, path: &str, ts_ms: i64) -> Result<Option<String>> {
        let since = self.last_gone(path, ts_ms)?.unwrap_or(i64::MIN);
        Ok(self
            .conn
            .query_row(
                "SELECT blob FROM live_changes WHERE path = ?1 AND ts_ms < ?2 AND ts_ms > ?3 AND blob IS NOT NULL ORDER BY ts_ms DESC, rowid DESC LIMIT 1",
                params![path, ts_ms, since],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// The newest symlink target recorded for `path` before `ts_ms`, and since it last went
    /// away.
    ///
    /// # Errors
    /// On SQLite errors.
    pub fn previous_target(&self, path: &str, ts_ms: i64) -> Result<Option<String>> {
        let since = self.last_gone(path, ts_ms)?.unwrap_or(i64::MIN);
        Ok(self
            .conn
            .query_row(
                "SELECT target FROM live_changes WHERE path = ?1 AND ts_ms < ?2 AND ts_ms > ?3 AND target IS NOT NULL ORDER BY ts_ms DESC, rowid DESC LIMIT 1",
                params![path, ts_ms, since],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// The newest dnf transaction that overlaps `from_s..=to_s` (seconds), from any computed
    /// interval.
    ///
    /// # Errors
    /// On SQLite errors.
    pub fn transaction_near(&self, from_s: i64, to_s: i64) -> Result<Option<Transaction>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, begin_s, end_s, user_id, description, state FROM transactions
                 WHERE begin_s <= ?2 AND COALESCE(end_s, begin_s) >= ?1
                 ORDER BY id DESC LIMIT 1",
                params![from_s, to_s],
                |r| {
                    Ok(Transaction {
                        id: r.get(0)?,
                        begin: r.get(1)?,
                        end: r.get(2)?,
                        user_id: r.get(3)?,
                        description: r.get(4)?,
                        state: r.get(5)?,
                    })
                },
            )
            .optional()?)
    }

    /// Stores the semantic summaries of a cause's changes, by path.
    ///
    /// # Errors
    /// On SQLite or encoding errors.
    pub fn store_summaries(
        &self,
        cause_id: i64,
        summaries: &[(String, Vec<String>)],
    ) -> Result<()> {
        let mut ins = self.conn.prepare(
            "INSERT OR REPLACE INTO summaries (cause_id, path, lines) VALUES (?1, ?2, ?3)",
        )?;
        for (path, lines) in summaries {
            if !lines.is_empty() {
                ins.execute(params![cause_id, path, serde_json::to_string(lines)?])?;
            }
        }
        Ok(())
    }

    /// A cause's summaries by path.
    ///
    /// # Errors
    /// On SQLite errors.
    pub fn summaries(
        &self,
        cause_id: i64,
    ) -> Result<std::collections::HashMap<String, Vec<String>>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, lines FROM summaries WHERE cause_id = ?1")?;
        let rows = stmt.query_map([cause_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (path, lines) = row?;
            out.insert(path, serde_json::from_str(&lines).unwrap_or_default());
        }
        Ok(out)
    }

    /// Records a window in which the live witness may have missed changes.
    ///
    /// # Errors
    /// On SQLite errors.
    pub fn add_gap(&mut self, start_ms: i64, end_ms: i64, reason: &str) -> Result<i64> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO gaps (start_ms, end_ms, reason) VALUES (?1, ?2, ?3)",
            params![start_ms, end_ms, reason],
        )?;
        let id = tx.last_insert_rowid();
        let gap = Gap {
            id,
            start_ms,
            end_ms,
            reason: reason.to_owned(),
        };
        Self::append_chain(&tx, "gap", id, &serde_json::to_string(&gap)?)?;
        tx.commit()?;
        Ok(id)
    }

    fn gap(&self, id: i64) -> Result<Option<Gap>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, start_ms, end_ms, reason FROM gaps WHERE id = ?1",
                [id],
                |r| {
                    Ok(Gap {
                        id: r.get(0)?,
                        start_ms: r.get(1)?,
                        end_ms: r.get(2)?,
                        reason: r.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    /// Gaps overlapping `[from_ms, to_ms]`.
    ///
    /// # Errors
    /// On SQLite errors.
    pub fn gaps_between(&self, from_ms: i64, to_ms: i64) -> Result<Vec<Gap>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, start_ms, end_ms, reason FROM gaps WHERE end_ms >= ?1 AND start_ms <= ?2 ORDER BY start_ms",
        )?;
        let rows = stmt.query_map([from_ms, to_ms], |r| {
            Ok(Gap {
                id: r.get(0)?,
                start_ms: r.get(1)?,
                end_ms: r.get(2)?,
                reason: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Stores which cause explains each change of a stored interval.
    ///
    /// # Errors
    /// On SQLite errors.
    pub fn store_attribution(&mut self, interval_id: i64, rows: &[Attribution]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM attribution WHERE interval_id = ?1",
            [interval_id],
        )?;
        {
            let mut ins = tx.prepare("INSERT INTO attribution (interval_id, path, cause_id, witness) VALUES (?1, ?2, ?3, ?4)")?;
            for (path, cause, witness) in rows {
                ins.execute(params![interval_id, path, cause, witness])?;
            }
        }
        let mut sorted = rows.to_vec();
        sorted.sort();
        let record = AttributionRecord {
            interval_id,
            rows: digest(&sorted)?,
        };
        Self::append_chain(
            &tx,
            "attribution",
            interval_id,
            &serde_json::to_string(&record)?,
        )?;
        tx.commit()?;
        Ok(())
    }

    fn attribution_rows(&self, interval_id: i64) -> Result<Vec<Attribution>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, cause_id, witness FROM attribution WHERE interval_id = ?1 ORDER BY path, cause_id, witness",
        )?;
        let rows = stmt.query_map([interval_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    fn attribution_of(
        &self,
        interval_id: i64,
    ) -> Result<std::collections::HashMap<String, (Option<CauseRef>, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT a.path, a.witness, c.id, c.kind, c.label FROM attribution a
             LEFT JOIN causes c ON c.id = a.cause_id WHERE a.interval_id = ?1",
        )?;
        let rows = stmt.query_map([interval_id], |r| {
            let id: Option<i64> = r.get(2)?;
            let cause = match id {
                Some(id) => Some(CauseRef {
                    id,
                    kind: r.get(3)?,
                    label: r.get(4)?,
                }),
                None => None,
            };
            Ok((r.get::<_, String>(0)?, (cause, r.get::<_, String>(1)?)))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Direct SQL access for tests that simulate tampering.
    #[doc(hidden)]
    #[must_use]
    pub fn raw(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn cp(id: i64, created_ms: i64, reason: &str) -> Checkpoint {
        Checkpoint {
            id,
            name: format!("c{id:06}"),
            created_ms,
            reason: reason.into(),
            generation: 100 + u64::try_from(id).unwrap(),
            boot_id: "b1".into(),
            note: None,
            pm_pid: None,
            pm_comm: None,
            pinned: false,
            pruned_ms: None,
        }
    }

    fn interval(from: i64, to: i64) -> Interval {
        Interval {
            from_id: from,
            to_id: to,
            computed_ms: 5,
            duration_ms: 40,
            stream_bytes: 1000,
            commands: 12,
            changed_bytes: 4096,
            changes: vec![
                Change {
                    path: "/etc/hosts".into(),
                    op: "modify".into(),
                    from: None,
                    kind: "file".into(),
                    class: Class::Config,
                    what: vec!["content".into()],
                    bytes: 4096,
                    cause: None,
                    witness: None,
                    summary: Vec::new(),
                },
                Change {
                    path: "/var/log/dnf5.log".into(),
                    op: "modify".into(),
                    from: None,
                    kind: "file".into(),
                    class: Class::Noise,
                    what: vec!["content".into(), "times".into()],
                    bytes: 0,
                    cause: None,
                    witness: None,
                    summary: Vec::new(),
                },
            ],
            packages: vec![PackageChange {
                name: "figlet".into(),
                arch: "x86_64".into(),
                action: "install".into(),
                from_evr: None,
                to_evr: Some("2.2.5-39.fc44".into()),
            }],
            transactions: vec![Transaction {
                id: 27,
                begin: 1,
                end: Some(2),
                user_id: 1000,
                description: "dnf install figlet".into(),
                state: "Ok".into(),
            }],
        }
    }

    #[test]
    fn checkpoints_intervals_and_chain() {
        let mut l = Ledger::open_in_memory().unwrap();
        assert_eq!(l.next_checkpoint_id().unwrap(), 1);
        l.add_checkpoint(&cp(1, 1000, "pre-dnf")).unwrap();
        l.add_checkpoint(&cp(2, 2000, "post-dnf")).unwrap();
        assert_eq!(l.next_checkpoint_id().unwrap(), 3);
        assert_eq!(l.pending_intervals().unwrap(), [(1, 2)]);
        l.store_interval(&interval(1, 2)).unwrap();
        assert_eq!(l.pending_intervals().unwrap(), []);

        let back = l.interval(1, 2).unwrap().unwrap();
        assert_eq!(back, interval(1, 2));
        let s = l.summary_ending_at(2).unwrap().unwrap();
        assert_eq!((s.counts.config, s.counts.noise, s.packages), (1, 1, 1));

        l.set_pinned(1, true).unwrap();
        l.mark_pruned(2, 9000).unwrap();
        assert_eq!(l.chain_len().unwrap(), 5);
        let v = l.verify().unwrap();
        assert!(v.ok, "{v:?}");
    }

    #[test]
    fn verify_detects_table_edits() {
        let mut l = Ledger::open_in_memory().unwrap();
        l.add_checkpoint(&cp(1, 1000, "manual")).unwrap();
        l.add_checkpoint(&cp(2, 2000, "manual")).unwrap();
        l.store_interval(&interval(1, 2)).unwrap();
        assert!(l.verify().unwrap().ok);

        l.raw()
            .execute(
                "UPDATE changes SET path = '/etc/shadow' WHERE path = '/etc/hosts'",
                [],
            )
            .unwrap();
        let v = l.verify().unwrap();
        assert!(!v.ok);
        assert_eq!(v.first_bad, Some(3));

        let l2 = Ledger::open_in_memory().unwrap();
        let mut l2 = l2;
        l2.add_checkpoint(&cp(1, 1000, "manual")).unwrap();
        l2.raw()
            .execute("UPDATE checkpoints SET pruned_ms = 5", [])
            .unwrap();
        assert!(!l2.verify().unwrap().ok);
    }

    #[test]
    fn verify_detects_chain_edits() {
        let mut l = Ledger::open_in_memory().unwrap();
        l.add_checkpoint(&cp(1, 1000, "manual")).unwrap();
        l.add_checkpoint(&cp(2, 2000, "manual")).unwrap();
        l.raw()
            .execute(
                "UPDATE chain SET record = replace(record, 'manual', 'boot') WHERE seq = 1",
                [],
            )
            .unwrap();
        let v = l.verify().unwrap();
        assert_eq!((v.ok, v.first_bad), (false, Some(1)));
    }

    fn cause(label: &str, started_ms: i64) -> Cause {
        Cause {
            id: 0,
            kind: "command".into(),
            label: label.into(),
            actor: Actor {
                pid: 42,
                comm: "sed".into(),
                exe: "/usr/bin/sed".into(),
                uid: 0,
                cgroup: "/user.slice".into(),
                command: vec!["sudo".into(), "sed".into()],
                via: None,
            },
            started_ms,
            ended_ms: started_ms + 10,
            changes: 0,
            package_paths: 0,
        }
    }

    fn live(path: &str, ts_ms: i64, blob: Option<&str>) -> LiveChange {
        LiveChange {
            ts_ms,
            path: path.into(),
            op: "modify".into(),
            from: None,
            kind: "file".into(),
            blob: blob.map(Into::into),
            size: Some(3),
            actor: None,
            target: None,
            summary: Vec::new(),
        }
    }

    #[test]
    fn previous_content_stops_where_the_path_went_away() {
        let mut l = Ledger::open_in_memory().unwrap();
        let gone = |path: &str, ts_ms: i64, op: &str, from: Option<&str>| LiveChange {
            op: op.into(),
            from: from.map(Into::into),
            blob: None,
            ..live(path, ts_ms, None)
        };
        let d = "/etc/x.service.d";
        l.store_cause(
            &cause("first run", 100),
            &[
                live("/etc/x.service.d/a.conf", 100, Some("old")),
                live("/etc/b.conf", 100, Some("b1")),
                live("/etc/c.conf", 100, Some("c1")),
            ],
        )
        .unwrap();
        l.store_cause(
            &cause("revert", 200),
            &[
                gone(d, 200, "delete", None),
                gone("/etc/b.conf", 200, "delete", None),
                gone("/etc/c2.conf", 200, "rename", Some("/etc/c.conf")),
            ],
        )
        .unwrap();
        // A directory above it was removed: the file in the second run is new.
        assert_eq!(
            l.last_gone("/etc/x.service.d/a.conf", 300).unwrap(),
            Some(200)
        );
        assert_eq!(
            l.previous_blob("/etc/x.service.d/a.conf", 300).unwrap(),
            None
        );
        assert_eq!(l.previous_blob("/etc/b.conf", 300).unwrap(), None);
        assert_eq!(
            l.previous_blob("/etc/c.conf", 300).unwrap(),
            None,
            "renamed away"
        );
        // Before it went away, the old content still counts; a sibling prefix doesn't match.
        assert_eq!(
            l.previous_blob("/etc/b.conf", 150).unwrap().as_deref(),
            Some("b1")
        );
        assert_eq!(l.last_gone("/etc/x.service.dd/a.conf", 300).unwrap(), None);
        l.store_cause(
            &cause("second run", 300),
            &[live("/etc/b.conf", 300, Some("b2"))],
        )
        .unwrap();
        assert_eq!(
            l.previous_blob("/etc/b.conf", 400).unwrap().as_deref(),
            Some("b2")
        );
    }

    #[test]
    fn causes_attribution_and_gaps() {
        let mut l = Ledger::open_in_memory().unwrap();
        l.add_checkpoint(&cp(1, 1000, "pre-dnf")).unwrap();
        l.add_checkpoint(&cp(2, 2000, "post-dnf")).unwrap();
        let cid = l
            .store_cause(
                &cause("sudo sed -i /etc/hosts", 1500),
                &[
                    live("/etc/hosts", 1500, Some("aa")),
                    live("/etc/hosts", 1600, Some("bb")),
                ],
            )
            .unwrap();
        let c = l.cause(cid).unwrap().unwrap();
        assert_eq!((c.changes, c.actor.comm.as_str()), (2, "sed"));
        assert_eq!(l.causes(10, None).unwrap().len(), 1);
        assert_eq!(l.live_changes_between(1400, 1700).unwrap().len(), 2);
        assert_eq!(
            l.previous_blob("/etc/hosts", 1600).unwrap().as_deref(),
            Some("aa")
        );
        assert_eq!(
            l.history("/etc/hosts", 5).unwrap()[0].1.blob.as_deref(),
            Some("bb")
        );

        let iid = l.store_interval(&interval(1, 2)).unwrap();
        l.store_attribution(
            iid,
            &[
                ("/etc/hosts".into(), Some(cid), "both".into()),
                ("/var/log/dnf5.log".into(), None, "state".into()),
            ],
        )
        .unwrap();
        let back = l.interval(1, 2).unwrap().unwrap();
        let hosts = back
            .changes
            .iter()
            .find(|c| c.path == "/etc/hosts")
            .unwrap();
        assert_eq!(hosts.cause.as_ref().map(|c| c.id), Some(cid));
        assert_eq!(hosts.witness.as_deref(), Some("both"));

        let g = l.add_gap(1700, 1800, "daemon down").unwrap();
        assert_eq!(l.gaps_between(1000, 2000).unwrap()[0].id, g);
        assert!(l.verify().unwrap().ok);

        l.raw()
            .execute("UPDATE live_changes SET blob = 'cc' WHERE blob = 'bb'", [])
            .unwrap();
        assert!(
            !l.verify().unwrap().ok,
            "editing a live change breaks its cause record"
        );
    }

    #[test]
    fn verify_detects_attribution_and_gap_edits() {
        let mut l = Ledger::open_in_memory().unwrap();
        l.add_checkpoint(&cp(1, 1000, "manual")).unwrap();
        l.add_checkpoint(&cp(2, 2000, "manual")).unwrap();
        let iid = l.store_interval(&interval(1, 2)).unwrap();
        l.store_attribution(iid, &[("/etc/hosts".into(), None, "state".into())])
            .unwrap();
        l.add_gap(1, 2, "overflow").unwrap();
        assert!(l.verify().unwrap().ok);
        l.raw()
            .execute("UPDATE attribution SET witness = 'both'", [])
            .unwrap();
        assert!(!l.verify().unwrap().ok);
        l.raw()
            .execute("UPDATE attribution SET witness = 'state'", [])
            .unwrap();
        assert!(l.verify().unwrap().ok);
        l.raw()
            .execute("UPDATE gaps SET reason = 'nothing to see'", [])
            .unwrap();
        assert!(!l.verify().unwrap().ok);
    }

    #[test]
    fn migrates_a_v1_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(SCHEMA).unwrap();
            c.pragma_update(None, "user_version", 1).unwrap();
        }
        let mut l = Ledger::open(&path).unwrap();
        l.add_checkpoint(&cp(1, 1, "boot")).unwrap();
        l.store_cause(&cause("x", 5), &[]).unwrap();
        let v: i64 = l
            .raw()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        assert!(l.verify().unwrap().ok);
    }

    #[test]
    fn package_sets_follow_links() {
        let mut l = Ledger::open_in_memory().unwrap();
        for id in 1..=3 {
            l.add_checkpoint(&cp(id, id * 1000, "manual")).unwrap();
        }
        l.store_package_set(1, Some(b"set-a"), None).unwrap();
        l.store_package_set(2, None, Some(1)).unwrap();
        l.store_package_set(3, None, Some(2)).unwrap();
        assert_eq!(l.package_set(3).unwrap(), Some((1, b"set-a".to_vec())));
        assert_eq!(l.package_set(4).unwrap(), None);
    }

    #[test]
    fn reopening_keeps_data_and_refuses_newer_schemas() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite");
        {
            let mut l = Ledger::open(&path).unwrap();
            l.add_checkpoint(&cp(1, 1, "boot")).unwrap();
        }
        let l = Ledger::open(&path).unwrap();
        assert_eq!(l.checkpoints().unwrap().len(), 1);
        l.raw().pragma_update(None, "user_version", 99).unwrap();
        drop(l);
        assert!(matches!(
            Ledger::open(&path),
            Err(LedgerError::TooNew { found: 99, .. })
        ));
    }
}
