//! The osreflogd D-Bus interface, as the CLI and the app use it: the raw proxy (methods return
//! JSON strings, as the daemon serves them) and typed calls for the read side.

use osreflog_model::{Cause, CauseDetail, DiffReport, LogEntry, Status};
use serde::de::DeserializeOwned;

#[zbus::proxy(
    interface = "io.github.khaledsaeed18.osreflog1",
    default_service = "io.github.khaledsaeed18.osreflog1",
    default_path = "/io/github/khaledsaeed18/osreflog1"
)]
pub trait Osreflog {
    fn status(&self) -> zbus::Result<String>;
    fn log(&self, limit: u32) -> zbus::Result<String>;
    fn diff(&self, from: &str, to: &str, all: bool) -> zbus::Result<String>;
    fn verify(&self) -> zbus::Result<String>;
    fn checkpoint(&self, note: &str) -> zbus::Result<String>;
    fn hook(&self, phase: &str, pid: u32) -> zbus::Result<String>;
    fn causes(&self, limit: u32) -> zbus::Result<String>;
    fn show(&self, id: i64) -> zbus::Result<String>;
    fn why(&self, path: &str, limit: u32) -> zbus::Result<String>;
    fn undo_plan(&self, id: i64) -> zbus::Result<String>;
    fn undo(&self, id: i64, digest: &str) -> zbus::Result<String>;
    fn bisect_run(&self, args: &str) -> zbus::Result<String>;
    fn bisect_status(&self) -> zbus::Result<String>;
    fn bisect_reset(&self) -> zbus::Result<String>;
    fn bisect_verdict(&self, verdict: &str) -> zbus::Result<String>;
    fn bisect_rearm(&self) -> zbus::Result<String>;
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// osreflogd isn't on the bus.
    #[error("osreflogd is not running (systemctl status osreflogd)")]
    NotRunning,
    /// The daemon refused or failed, with its message.
    #[error("{0}")]
    Daemon(String),
    #[error("D-Bus: {0}")]
    Bus(zbus::Error),
    #[error("osreflogd's answer didn't parse: {0}")]
    Json(#[from] serde_json::Error),
}

impl ClientError {
    /// Whether the daemon isn't there at all (so a user interface can say so plainly).
    #[must_use]
    pub fn not_running(&self) -> bool {
        matches!(self, ClientError::NotRunning)
    }
}

/// A D-Bus error, as a person should read it.
#[must_use]
pub fn explain(e: zbus::Error) -> ClientError {
    match e {
        zbus::Error::MethodError(name, Some(msg), _)
            if name.as_str().starts_with(osreflog_model::bus::ERROR_PREFIX) =>
        {
            ClientError::Daemon(msg)
        }
        zbus::Error::MethodError(name, _, _)
            if matches!(
                name.as_str(),
                "org.freedesktop.DBus.Error.ServiceUnknown"
                    | "org.freedesktop.DBus.Error.NameHasNoOwner"
            ) =>
        {
            ClientError::NotRunning
        }
        other => ClientError::Bus(other),
    }
}

/// A connection to osreflogd with typed calls.
#[derive(Clone)]
pub struct Client {
    proxy: OsreflogProxy<'static>,
}

fn parse<T: DeserializeOwned>(raw: &str) -> Result<T, ClientError> {
    Ok(serde_json::from_str(raw)?)
}

impl Client {
    /// On the system bus (`session`: the session bus, for development).
    ///
    /// # Errors
    /// If D-Bus can't be reached.
    pub async fn connect(session: bool) -> Result<Self, ClientError> {
        let connection = if session {
            zbus::Connection::session().await
        } else {
            zbus::Connection::system().await
        }
        .map_err(ClientError::Bus)?;
        let proxy = OsreflogProxy::new(&connection)
            .await
            .map_err(ClientError::Bus)?;
        Ok(Self { proxy })
    }

    #[must_use]
    pub fn proxy(&self) -> &OsreflogProxy<'static> {
        &self.proxy
    }

    /// # Errors
    /// If the call fails or its answer doesn't parse.
    pub async fn status(&self) -> Result<Status, ClientError> {
        parse(&self.proxy.status().await.map_err(explain)?)
    }

    /// The newest `limit` causes, newest first.
    ///
    /// # Errors
    /// If the call fails or its answer doesn't parse.
    pub async fn causes(&self, limit: u32) -> Result<Vec<Cause>, ClientError> {
        parse(&self.proxy.causes(limit).await.map_err(explain)?)
    }

    /// # Errors
    /// If the call fails or its answer doesn't parse.
    pub async fn show(&self, id: i64) -> Result<CauseDetail, ClientError> {
        parse(&self.proxy.show(id).await.map_err(explain)?)
    }

    /// The newest `limit` checkpoints with their interval summaries.
    ///
    /// # Errors
    /// If the call fails or its answer doesn't parse.
    pub async fn log(&self, limit: u32) -> Result<Vec<LogEntry>, ClientError> {
        parse(&self.proxy.log(limit).await.map_err(explain)?)
    }

    /// # Errors
    /// If the call fails or its answer doesn't parse.
    pub async fn diff(&self, from: &str, to: &str, all: bool) -> Result<DiffReport, ClientError> {
        parse(&self.proxy.diff(from, to, all).await.map_err(explain)?)
    }
}
