//! The window never waits on osreflogd: D-Bus calls run on a small tokio runtime in a thread of
//! their own (the workspace's `zbus` uses tokio), and their answers come back over a channel
//! the GTK main loop reads.

use osreflog_client::{Client, ClientError};
use osreflog_model::{Cause, CauseDetail, Status};

/// How many causes the timeline asks for.
pub const LIMIT: u32 = 500;

#[derive(Debug)]
pub enum Request {
    /// The status and the newest causes.
    Refresh,
    /// One cause with its changes.
    Show(i64),
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

#[derive(Debug)]
pub enum Reply {
    History(Result<(Status, Vec<Cause>), Failure>),
    Detail(i64, Result<CauseDetail, Failure>),
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

async fn serve(requests: async_channel::Receiver<Request>, replies: async_channel::Sender<Reply>) {
    let mut client: Option<Client> = None;
    while let Ok(req) = requests.recv().await {
        // Connect once and keep the connection; drop it after a failure, so a daemon that
        // comes back (or a bus that restarted) is found again on the next request.
        if client.is_none() {
            client = Client::connect(false).await.ok();
        }
        let reply = match (&client, req) {
            (None, Request::Refresh) => Reply::History(Err(Failure {
                down: true,
                message: "the system bus isn't reachable".into(),
            })),
            (None, Request::Show(id)) => Reply::Detail(
                id,
                Err(Failure {
                    down: true,
                    message: "the system bus isn't reachable".into(),
                }),
            ),
            (Some(c), Request::Refresh) => {
                let history =
                    async { Ok::<_, ClientError>((c.status().await?, c.causes(LIMIT).await?)) };
                Reply::History(history.await.map_err(Failure::from))
            }
            (Some(c), Request::Show(id)) => {
                Reply::Detail(id, c.show(id).await.map_err(Failure::from))
            }
        };
        if matches!(&reply, Reply::History(Err(_)) | Reply::Detail(_, Err(_))) {
            client = None;
        }
        if replies.send(reply).await.is_err() {
            return; // the window is gone
        }
    }
}
