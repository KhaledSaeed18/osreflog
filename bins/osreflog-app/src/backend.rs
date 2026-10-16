//! The window never waits on osreflogd: D-Bus calls run on a small tokio runtime in a thread of
//! their own (the workspace's `zbus` uses tokio), and their answers come back over a channel
//! the GTK main loop reads.

use osreflog_client::{Client, ClientError};
use osreflog_model::{Cause, CauseDetail, DiffReport, LogEntry, Status};

/// How many causes the timeline asks for.
pub const LIMIT: u32 = 500;
/// How many checkpoints the checkpoint list asks for.
pub const CHECKPOINTS: u32 = 300;

#[derive(Debug)]
pub enum Request {
    /// The status, the newest causes and the newest checkpoints.
    Refresh,
    /// One cause with its changes.
    Show(i64),
    /// What changed from one checkpoint to another (`@{now}`: the live system). `all`: package
    /// files and noise too.
    Diff {
        from: String,
        to: String,
        all: bool,
    },
    /// What undoing a cause would do (nothing is written).
    UndoPlan(i64),
    /// Undo a cause exactly as the preview with this digest showed it; polkit asks.
    Undo {
        id: i64,
        digest: String,
    },
    /// The current or last bisect.
    BisectStatus,
    /// Start a bisect; polkit asks.
    BisectRun(Box<osreflog_bisect::Args>),
    /// `good`, `bad` or `skip` for the waiting probe.
    BisectVerdict(String),
    BisectRearm,
    BisectReset,
    /// Take back the last verdict of a manual bisect.
    BisectUndoVerdict,
}

/// Why a call has no answer: the daemon isn't there, or something else went wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub down: bool,
    pub message: String,
}

impl From<ClientError> for Failure {
    fn from(e: ClientError) -> Self {
        Failure {
            down: e.not_running(),
            message: e.to_string(),
        }
    }
}

/// The status, the timeline's causes, the checkpoint list and the current or last bisect.
pub type History = (
    Status,
    Vec<Cause>,
    Vec<LogEntry>,
    Option<osreflog_bisect::Status>,
);

#[derive(Debug)]
pub enum Reply {
    History(Result<Box<History>, Failure>),
    Detail(i64, Result<CauseDetail, Failure>),
    Diff {
        from: String,
        to: String,
        all: bool,
        result: Result<Box<DiffReport>, Failure>,
    },
    UndoPlan(i64, Result<Box<osreflog_undo::Preview>, Failure>),
    Undone(i64, Result<Box<osreflog_undo::Applied>, Failure>),
    Bisect(Result<Option<Box<osreflog_bisect::Status>>, Failure>),
    /// A bisect action (start, verdict, rearm, reset) was refused or failed; a status follows
    /// every action either way.
    BisectRefused(String, Failure),
}

/// Starts the thread. Requests go in through the first channel, replies come out of the second.
#[must_use]
pub fn spawn() -> (
    async_channel::Sender<Request>,
    async_channel::Receiver<Reply>,
) {
    let (req_tx, req_rx) = async_channel::unbounded::<Request>();
    let (rep_tx, rep_rx) = async_channel::unbounded::<Reply>();
    let spawned = std::thread::Builder::new()
        .name("osreflog-dbus".into())
        .spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            rt.block_on(serve(req_rx, rep_tx));
        });
    if let Err(e) = spawned {
        eprintln!("osreflog-app: starting the D-Bus thread: {e}");
    }
    (req_tx, rep_rx)
}

fn unreachable_bus() -> Failure {
    Failure {
        down: true,
        message: "the system bus isn't reachable".into(),
    }
}

async fn history(c: &Client) -> Result<History, ClientError> {
    Ok((
        c.status().await?,
        c.causes(LIMIT).await?,
        c.log(CHECKPOINTS).await?,
        c.bisect_status().await?,
    ))
}

async fn serve(requests: async_channel::Receiver<Request>, replies: async_channel::Sender<Reply>) {
    let mut client: Option<Client> = None;
    while let Ok(req) = requests.recv().await {
        // Connect once and keep the connection; drop it after a failure, so a daemon that
        // comes back (or a bus that restarted) is found again on the next request.
        if client.is_none() {
            client = Client::connect(false).await.ok();
        }
        let reply = match (&client, req) {
            (None, Request::Refresh) => Reply::History(Err(unreachable_bus())),
            (None, Request::Show(id)) => Reply::Detail(id, Err(unreachable_bus())),
            (None, Request::UndoPlan(id)) => Reply::UndoPlan(id, Err(unreachable_bus())),
            (None, Request::BisectStatus) => Reply::Bisect(Err(unreachable_bus())),
            (None, Request::BisectRun(_)) => {
                Reply::BisectRefused("start".into(), unreachable_bus())
            }
            (
                None,
                Request::BisectVerdict(_)
                | Request::BisectRearm
                | Request::BisectReset
                | Request::BisectUndoVerdict,
            ) => Reply::BisectRefused("act".into(), unreachable_bus()),
            (None, Request::Undo { id, .. }) => Reply::Undone(id, Err(unreachable_bus())),
            (None, Request::Diff { from, to, all }) => Reply::Diff {
                from,
                to,
                all,
                result: Err(unreachable_bus()),
            },
            (Some(c), Request::Refresh) => {
                Reply::History(history(c).await.map(Box::new).map_err(Failure::from))
            }
            (Some(c), Request::Show(id)) => {
                Reply::Detail(id, c.show(id).await.map_err(Failure::from))
            }
            (Some(c), Request::BisectStatus) => Reply::Bisect(
                c.bisect_status()
                    .await
                    .map(|s| s.map(Box::new))
                    .map_err(Failure::from),
            ),
            (Some(c), Request::BisectRun(args)) => {
                bisect_action(c, &replies, "start", c.bisect_run(&args).await.map(|_| ())).await
            }
            (Some(c), Request::BisectVerdict(v)) => {
                bisect_action(c, &replies, "verdict", c.bisect_verdict(&v).await).await
            }
            (Some(c), Request::BisectRearm) => {
                bisect_action(c, &replies, "rearm", c.bisect_rearm().await).await
            }
            (Some(c), Request::BisectUndoVerdict) => {
                bisect_action(c, &replies, "undo-verdict", c.bisect_undo_verdict().await).await
            }
            (Some(c), Request::BisectReset) => {
                bisect_action(c, &replies, "reset", c.bisect_reset().await).await
            }
            (Some(c), Request::UndoPlan(id)) => Reply::UndoPlan(
                id,
                c.undo_plan(id).await.map(Box::new).map_err(Failure::from),
            ),
            (Some(c), Request::Undo { id, digest }) => Reply::Undone(
                id,
                c.undo(id, &digest)
                    .await
                    .map(Box::new)
                    .map_err(Failure::from),
            ),
            (Some(c), Request::Diff { from, to, all }) => {
                let result = c
                    .diff(&from, &to, all)
                    .await
                    .map(Box::new)
                    .map_err(Failure::from);
                Reply::Diff {
                    from,
                    to,
                    all,
                    result,
                }
            }
        };
        let failed = match &reply {
            Reply::History(r) => r.as_ref().err(),
            Reply::Detail(_, r) => r.as_ref().err(),
            Reply::Diff { result, .. } => result.as_ref().err(),
            Reply::UndoPlan(_, r) => r.as_ref().err(),
            Reply::Undone(_, r) => r.as_ref().err(),
            Reply::Bisect(r) => r.as_ref().err(),
            Reply::BisectRefused(_, f) => Some(f),
        };
        if failed.is_some_and(|f| f.down) {
            client = None;
        }
        if replies.send(reply).await.is_err() {
            return; // the window is gone
        }
    }
}

/// After a bisect action: its refusal, if it was refused, then the session as it is now.
async fn bisect_action(
    c: &Client,
    replies: &async_channel::Sender<Reply>,
    what: &str,
    result: Result<(), ClientError>,
) -> Reply {
    if let Err(e) = result {
        let _ = replies
            .send(Reply::BisectRefused(what.to_owned(), Failure::from(e)))
            .await;
    }
    Reply::Bisect(
        c.bisect_status()
            .await
            .map(|s| s.map(Box::new))
            .map_err(Failure::from),
    )
}
