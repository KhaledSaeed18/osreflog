//! Running the semantic summarizer: the worker side (inside the sandbox) and the daemon side
//! (spawning the worker, with a deadline).

use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use osreflog_semantic::{Item, summarize_all};
use serde::{Deserialize, Serialize};

pub const WORKER_FLAG: &str = "--summarize-worker";
const MAX_REQUEST: u64 = 64 << 20;
const DEADLINE: Duration = Duration::from_secs(10);

#[derive(Serialize, Deserialize)]
struct Request {
    items: Vec<Item>,
}

#[derive(Serialize, Deserialize)]
struct Response {
    summaries: Vec<Vec<String>>,
}

/// The worker: enter the sandbox, then read a request from stdin and write summaries to
/// stdout. Exit code 2 if the sandbox can't be entered (nothing is read then).
#[must_use]
pub fn worker_main() -> i32 {
    if let Err(e) = crate::sandbox::enter() {
        let _ = writeln!(io::stderr(), "osreflogd worker: sandbox unavailable: {e}");
        return 2;
    }
    let mut input = Vec::new();
    if io::stdin()
        .take(MAX_REQUEST)
        .read_to_end(&mut input)
        .is_err()
    {
        return 3;
    }
    let Ok(req) = serde_json::from_slice::<Request>(&input) else {
        return 4;
    };
    let out = Response {
        summaries: summarize_all(&req.items),
    };
    let Ok(bytes) = serde_json::to_vec(&out) else {
        return 5;
    };
    let mut stdout = io::stdout().lock();
    if stdout
        .write_all(&bytes)
        .and_then(|()| stdout.flush())
        .is_err()
    {
        return 6;
    }
    0
}

/// Summarizes `items` in a sandboxed worker process (this same executable).
///
/// # Errors
/// If the worker can't start, fails, is killed by its sandbox, or misses the deadline.
pub fn run(items: Vec<Item>) -> io::Result<Vec<Vec<String>>> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let exe = std::env::current_exe()?;
    let mut child = Command::new(exe)
        .arg(WORKER_FLAG)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let request = serde_json::to_vec(&Request { items }).map_err(io::Error::other)?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("no stdin"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("no stdout"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("no stderr"))?;
    let writer = std::thread::spawn(move || stdin.write_all(&request));
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut err = String::new();
        let r = stdout.read_to_end(&mut out);
        let _ = stderr.read_to_string(&mut err);
        let _ = tx.send((r, out, err));
    });
    let Ok((read, out, err)) = rx.recv_timeout(DEADLINE) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "summarizer worker missed its deadline",
        ));
    };
    let _ = writer.join();
    let status = child.wait()?;
    read?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "summarizer worker failed ({status}): {}",
            err.trim()
        )));
    }
    let resp: Response = serde_json::from_slice(&out).map_err(io::Error::other)?;
    Ok(resp.summaries)
}
