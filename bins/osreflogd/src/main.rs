//! osreflogd: takes checkpoints (at boot, around package transactions, daily, on request),
//! diffs them with btrfs send, and serves the history on the system bus.
//!
//! Runs as root in its own mount namespace (systemd `PrivateMounts=yes`), where it mounts the
//! btrfs top level; see `data/systemd/osreflogd.service`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use osreflog_ledger::Ledger;
use osreflog_model::bus;
use osreflogd::engine::Engine;
use osreflogd::layout::{DEFAULT_TOP, Layout};
use osreflogd::service::Service;
use tokio::sync::Notify;
use tracing::Level;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// How often the background worker looks for due work even without a wake-up.
const TICK: Duration = Duration::from_secs(3600);

#[derive(Parser)]
#[command(version, about = "osreflog daemon")]
struct Args {
    /// The mount point of the btrfs subvolume to track.
    #[arg(long, default_value = "/")]
    root: PathBuf,
    /// Where to mount the btrfs top level (inside this process's mount namespace).
    #[arg(long, default_value = DEFAULT_TOP)]
    top: PathBuf,
    /// Serve on the session bus instead of the system bus (development only).
    #[arg(long)]
    session_bus: bool,
}

fn main() -> anyhow::Result<()> {
    // The sandboxed summarizer: this same executable, before anything else runs.
    if std::env::args().nth(1).as_deref() == Some(osreflogd::summarize::WORKER_FLAG) {
        std::process::exit(osreflogd::summarize::worker_main());
    }
    // Test hook: enter the sandbox, then try to read a file. The sandbox must kill us first.
    if std::env::args().nth(1).as_deref() == Some("--sandbox-selftest") {
        if let Err(e) = osreflogd::sandbox::enter() {
            eprintln!("sandbox unavailable: {e}");
            std::process::exit(2);
        }
        let escaped = std::fs::read("/etc/hostname").is_ok();
        std::process::exit(i32::from(!escaped));
    }
    let args = Args::parse();
    init_logging();
    anyhow::ensure!(
        rustix::process::geteuid().is_root(),
        "osreflogd must run as root"
    );

    let layout = Layout::open(&args.root, &args.top).context("setting up the osreflog layout")?;
    let ledger = Ledger::open(&layout.ledger_path()).context("opening the ledger")?;
    let engine = Arc::new(Engine::new(layout, ledger).context("starting the engine")?);
    engine.start_summarizer();
    if let Some(c) = engine
        .boot_checkpoint_if_needed()
        .context("taking the boot checkpoint")?
    {
        tracing::info!(id = c.id, "boot checkpoint");
    }

    let top = engine.layout().top.clone();
    let subvolume = engine.layout().subvolume.clone();
    let capture = match osreflogd::capture::spawn(
        Arc::clone(&engine),
        Arc::clone(&engine.live),
        &top,
        &subvolume,
        &engine.blob_dir(),
    ) {
        Ok(handle) => {
            engine
                .live_started()
                .context("recording the live witness start")?;
            Some(handle)
        }
        Err(e) => {
            tracing::error!("live witness unavailable, running with the state witness only: {e:#}");
            None
        }
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let served = runtime.block_on(serve(Arc::clone(&engine), args.session_bus));
    if let Some(handle) = capture {
        engine
            .live
            .stop
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = handle.join();
        engine.live_stopped();
    }
    served
}

async fn serve(engine: Arc<Engine>, session_bus: bool) -> anyhow::Result<()> {
    let wake = Arc::new(Notify::new());
    let connection = if session_bus {
        zbus::Connection::session().await
    } else {
        zbus::Connection::system().await
    }
    .context("connecting to D-Bus")?;
    connection
        .object_server()
        .at(
            bus::PATH,
            Service::new(Arc::clone(&engine), Arc::clone(&wake)),
        )
        .await
        .context("exporting the interface")?;
    {
        let live = Arc::clone(&engine.live);
        tokio::spawn(async move {
            if let Err(e) = osreflogd::delegation::run(live).await {
                tracing::error!("D-Bus caller attribution unavailable: {e}");
            }
        });
    }
    connection
        .request_name(bus::NAME)
        .await
        .with_context(|| format!("acquiring {}", bus::NAME))?;
    tracing::info!("serving {}", bus::NAME);

    // Diffs pending from before a restart, then whenever woken or every TICK.
    wake.notify_one();
    let worker = tokio::spawn(async move {
        loop {
            tokio::select! {
                () = wake.notified() => {}
                () = tokio::time::sleep(TICK) => {}
            }
            let engine = Arc::clone(&engine);
            let result = tokio::task::spawn_blocking(move || {
                engine.scheduled_checkpoint_if_due()?;
                engine.compute_pending()
            })
            .await;
            match result {
                Ok(Ok(0)) => {}
                Ok(Ok(n)) => tracing::info!(intervals = n, "computed pending diffs"),
                Ok(Err(e)) => tracing::error!(err = %e, "background work failed"),
                Err(e) => tracing::error!(err = %e, "background worker panicked"),
            }
        }
    });

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = term.recv() => tracing::info!("SIGTERM, exiting"),
        _ = tokio::signal::ctrl_c() => tracing::info!("interrupted, exiting"),
    }
    worker.abort();
    Ok(())
}

/// Logs to the journal under systemd, to stderr otherwise. zbus traces every message below
/// `warn`.
fn init_logging() {
    let filter = Targets::new()
        .with_default(Level::INFO)
        .with_target("zbus", Level::WARN);
    let registry = tracing_subscriber::registry().with(filter);
    if std::env::var_os("JOURNAL_STREAM").is_some()
        && let Ok(journald) = tracing_journald::layer()
    {
        registry.with(journald).init();
    } else {
        registry.with(tracing_subscriber::fmt::layer()).init();
    }
}
