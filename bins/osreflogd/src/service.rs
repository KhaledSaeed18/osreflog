//! The `io.github.khaledsaeed18.osreflog1` D-Bus interface. Payloads are JSON strings (the
//! types in `osreflog-model`), the same JSON the CLI prints with `--json`.

use std::sync::Arc;

use osreflog_model::{bus, reason};
use tokio::sync::Notify;
use zbus::message::Header;

use crate::auth;
use crate::engine::{Engine, EngineError};

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "io.github.khaledsaeed18.osreflog1.Error")]
pub enum Error {
    #[zbus(error)]
    ZBus(zbus::Error),
    /// The caller may not do this.
    NotAuthorized(String),
    /// A bad reference or argument.
    Invalid(String),
    /// Anything else; details are in the daemon's journal.
    Failed(String),
}

impl From<EngineError> for Error {
    fn from(e: EngineError) -> Self {
        match e {
            EngineError::Ref(_) | EngineError::Invalid(_) => Error::Invalid(e.to_string()),
            other => {
                tracing::error!(err = %other, "request failed");
                Error::Failed(other.to_string())
            }
        }
    }
}

pub struct Service {
    engine: Arc<Engine>,
    /// Wakes the background worker to compute pending intervals.
    pub wake: Arc<Notify>,
    /// The current or last bisect session.
    bisect: crate::bisect::Shared,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Need {
    Read,
    Checkpoint,
    Undo,
    Bisect,
    Root,
}

impl Service {
    #[must_use]
    pub fn new(engine: Arc<Engine>, wake: Arc<Notify>) -> Self {
        Self {
            engine,
            wake,
            bisect: crate::bisect::Shared::default(),
        }
    }

    async fn authorize(
        &self,
        header: &Header<'_>,
        connection: &zbus::Connection,
        need: Need,
    ) -> Result<u32, Error> {
        let caller = header
            .sender()
            .map(|n| n.as_str().to_owned())
            .ok_or_else(|| Error::NotAuthorized("no sender".into()))?;
        let uid = auth::caller_uid(connection, &caller).await.map_err(|e| {
            tracing::error!(%e, "looking up the caller's uid failed");
            Error::Failed("identifying the caller failed".into())
        })?;
        if uid == 0 {
            return Ok(uid);
        }
        let (action, interactive) = match need {
            Need::Root => return Err(Error::NotAuthorized("only root may call this".into())),
            Need::Read => (bus::ACTION_READ, false),
            Need::Checkpoint => (bus::ACTION_CHECKPOINT, true),
            Need::Undo => (bus::ACTION_UNDO, true),
            Need::Bisect => (bus::ACTION_BISECT, true),
        };
        let allowed = auth::polkit_allows(connection, &caller, action, interactive)
            .await
            .map_err(|e| {
                tracing::error!(%e, "polkit check failed");
                Error::Failed("authorization check failed".into())
            })?;
        if allowed {
            Ok(uid)
        } else {
            tracing::warn!(uid, action, "denied by polkit");
            Err(Error::NotAuthorized(format!(
                "not allowed by polkit ({action})"
            )))
        }
    }

    async fn run<T, F>(&self, f: F) -> Result<String, Error>
    where
        T: serde::Serialize + Send + 'static,
        F: FnOnce(&Engine) -> Result<T, EngineError> + Send + 'static,
    {
        let engine = Arc::clone(&self.engine);
        let value = tokio::task::spawn_blocking(move || f(&engine))
            .await
            .map_err(|e| Error::Failed(format!("worker failed: {e}")))??;
        serde_json::to_string(&value).map_err(|e| Error::Failed(e.to_string()))
    }
}

#[zbus::interface(name = "io.github.khaledsaeed18.osreflog1")]
impl Service {
    /// Daemon and ledger status (`Status` as JSON).
    async fn status(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] c: &zbus::Connection,
    ) -> Result<String, Error> {
        self.authorize(&header, c, Need::Read).await?;
        self.run(Engine::status).await
    }

    /// The newest `limit` checkpoints with interval summaries (`[LogEntry]` as JSON).
    async fn log(
        &self,
        limit: u32,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] c: &zbus::Connection,
    ) -> Result<String, Error> {
        self.authorize(&header, c, Need::Read).await?;
        let limit = usize::try_from(limit)
            .unwrap_or(usize::MAX)
            .clamp(1, 10_000);
        self.run(move |e| e.log(limit)).await
    }

    /// What changed between two references (`DiffReport` as JSON). `all` includes
    /// package-owned and noise paths.
    async fn diff(
        &self,
        from: String,
        to: String,
        all: bool,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] c: &zbus::Connection,
    ) -> Result<String, Error> {
        self.authorize(&header, c, Need::Read).await?;
        self.run(move |e| e.diff(&from, &to, all)).await
    }

    /// Re-hashes the ledger's chain (`VerifyReport` as JSON).
    async fn verify(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] c: &zbus::Connection,
    ) -> Result<String, Error> {
        self.authorize(&header, c, Need::Read).await?;
        self.run(Engine::verify).await
    }

    /// Takes a manual checkpoint (`Checkpoint` as JSON).
    async fn checkpoint(
        &self,
        note: String,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] c: &zbus::Connection,
    ) -> Result<String, Error> {
        let uid = self.authorize(&header, c, Need::Checkpoint).await?;
        let note = if note.is_empty() {
            format!("by uid {uid}")
        } else {
            note
        };
        let out = self
            .run(move |e| e.checkpoint(reason::MANUAL, Some(note), None))
            .await;
        self.wake.notify_one();
        out
    }

    /// Called by the dnf5 actions hook (root only): `pre` or `post`, with the package
    /// manager's pid. Replies once the snapshot exists; the diff follows in the background.
    async fn hook(
        &self,
        phase: String,
        pid: u32,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] c: &zbus::Connection,
    ) -> Result<String, Error> {
        self.authorize(&header, c, Need::Root).await?;
        let why = match phase.as_str() {
            "pre" => reason::PRE_DNF,
            "post" => reason::POST_DNF,
            _ => return Err(Error::Invalid(format!("unknown hook phase {phase:?}"))),
        };
        if why == reason::PRE_DNF {
            let label = std::fs::read_to_string(format!("/proc/{pid}/comm"))
                .map(|s| s.trim().to_owned())
                .unwrap_or_default();
            self.engine.live.bracket_open(pid, label);
        }
        let out = self.run(move |e| e.checkpoint(why, None, Some(pid))).await;
        if why == reason::POST_DNF {
            self.engine.live.bracket_close(pid);
            self.wake.notify_one();
        }
        out
    }

    /// The newest causes the live witness recorded (`[Cause]` as JSON).
    async fn causes(
        &self,
        limit: u32,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] c: &zbus::Connection,
    ) -> Result<String, Error> {
        self.authorize(&header, c, Need::Read).await?;
        let limit = usize::try_from(limit)
            .unwrap_or(usize::MAX)
            .clamp(1, 10_000);
        self.run(move |e| e.causes(limit)).await
    }

    /// One cause with all its changes (`CauseDetail` as JSON).
    async fn show(
        &self,
        id: i64,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] c: &zbus::Connection,
    ) -> Result<String, Error> {
        self.authorize(&header, c, Need::Read).await?;
        self.run(move |e| e.cause_detail(id)).await
    }

    /// What undoing cause `id` would do, judged against the live system now (`Preview` as
    /// JSON). Nothing is written.
    async fn undo_plan(
        &self,
        id: i64,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] c: &zbus::Connection,
    ) -> Result<String, Error> {
        self.authorize(&header, c, Need::Read).await?;
        self.run(move |e| crate::undo::preview(e, id).map(|(_, p)| p))
            .await
    }

    /// Undoes cause `id`, if its plan still has `digest` (the one `UndoPlan` showed): a
    /// verified `pre-undo` checkpoint first, then the sandboxed worker (`Applied` as JSON).
    async fn undo(
        &self,
        id: i64,
        digest: String,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] c: &zbus::Connection,
    ) -> Result<String, Error> {
        let uid = self.authorize(&header, c, Need::Undo).await?;
        tracing::info!(uid, cause = id, "undo requested");
        let engine = Arc::clone(&self.engine);
        let mut applied =
            tokio::task::spawn_blocking(move || crate::undo::apply(&engine, id, &digest))
                .await
                .map_err(|e| Error::Failed(format!("worker failed: {e}")))??;
        if applied.reloaded_systemd
            && let Err(e) = reload_systemd(c).await
        {
            tracing::warn!(%e, "reloading systemd after the undo failed");
            applied.reloaded_systemd = false;
        }
        tracing::info!(
            cause = id,
            restored = applied.report.restored(),
            conflicts = applied.report.conflicts(),
            "undo applied"
        );
        self.wake.notify_one();
        serde_json::to_string(&applied).map_err(|e| Error::Failed(e.to_string()))
    }

    /// Starts a bisect (`osreflog_bisect::Args` as JSON) in the background; returns its
    /// `Status`. One at a time.
    async fn bisect_run(
        &self,
        args: String,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] c: &zbus::Connection,
    ) -> Result<String, Error> {
        let uid = self.authorize(&header, c, Need::Bisect).await?;
        let args: osreflog_bisect::Args = serde_json::from_str(&args)
            .map_err(|e| Error::Invalid(format!("bad arguments: {e}")))?;
        tracing::info!(uid, good = %args.good, bad = %args.bad, tier = ?args.tier, "bisect requested");
        let status = crate::bisect::start(Arc::clone(&self.engine), &self.bisect, args)?;
        serde_json::to_string(&status).map_err(|e| Error::Failed(e.to_string()))
    }

    /// The current or last bisect (`Status` as JSON, or `null`).
    async fn bisect_status(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] c: &zbus::Connection,
    ) -> Result<String, Error> {
        self.authorize(&header, c, Need::Read).await?;
        let status = self
            .bisect
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        serde_json::to_string(&status).map_err(|e| Error::Failed(e.to_string()))
    }

    /// Forgets the last bisect; refused while one is running.
    async fn bisect_reset(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] c: &zbus::Connection,
    ) -> Result<String, Error> {
        self.authorize(&header, c, Need::Bisect).await?;
        let mut s = self
            .bisect
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if s.as_ref().is_some_and(|s| s.running) {
            return Err(Error::Invalid(
                "a bisect is running; wait for it to finish".into(),
            ));
        }
        *s = None;
        Ok("null".into())
    }

    /// Why a path changed: its newest live changes, causes, and content before and after
    /// (`[WhyEntry]` as JSON). Secrets are never stored, so never returned.
    async fn why(
        &self,
        path: String,
        limit: u32,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] c: &zbus::Connection,
    ) -> Result<String, Error> {
        self.authorize(&header, c, Need::Read).await?;
        let limit = usize::try_from(limit).unwrap_or(usize::MAX).clamp(1, 1000);
        self.run(move |e| e.why(&path, limit)).await
    }
}

/// Reloads systemd after unit files or enablement links were restored, as `systemctl
/// daemon-reload` does.
async fn reload_systemd(connection: &zbus::Connection) -> zbus::Result<()> {
    connection
        .call_method(
            Some("org.freedesktop.systemd1"),
            "/org/freedesktop/systemd1",
            Some("org.freedesktop.systemd1.Manager"),
            "Reload",
            &(),
        )
        .await
        .map(|_| ())
}

/// Compatibility check that the interface name matches the shared constant.
const _: () = assert!(bus::INTERFACE.len() == "io.github.khaledsaeed18.osreflog1".len());
